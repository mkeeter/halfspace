use crate::{App, AppState, Message, MessageSender, Modal, state, wgpu_setup};
use log::{error, info, warn};
use wasm_bindgen::prelude::*;

/// Re-export init_thread_pool to be called on the web
pub use wasm_bindgen_rayon::init_thread_pool;

use eframe::wasm_bindgen::JsCast;

// YOLO zone
unsafe impl Sync for crate::painters::WgpuResources {}
unsafe impl Send for crate::painters::WgpuResources {}
unsafe impl Send for crate::WgpuError {}
unsafe impl Sync for crate::WgpuError {}

fn get_canvas() -> web_sys::HtmlCanvasElement {
    let window = web_sys::window().expect("No window");
    let document = window.document().expect("No document");

    document
        .get_element_by_id("the_canvas_id")
        .expect("Failed to find the_canvas_id")
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .expect("the_canvas_id was not a HtmlCanvasElement")
}

fn custom_panic_handler(info: &std::panic::PanicHookInfo) {
    let window = web_sys::window().expect("No window");
    let document = window.document().expect("No document");
    let p = document.get_element_by_id("panic-message").unwrap();
    p.set_text_content(Some(&format!("{info}")));

    let div = document.get_element_by_id("wasm-panic").unwrap();
    div.remove_attribute("hidden").unwrap();

    get_canvas().remove();
}

#[wasm_bindgen]
pub fn run() {
    let window = web_sys::window().expect("No window");
    let document = window.document().expect("No document");
    let location = window.location();

    let loading = document.get_element_by_id("loading").unwrap();
    loading.remove();

    let params = location
        .search()
        .and_then(|s| web_sys::UrlSearchParams::new_with_str(&s))
        .ok();

    // Get an optional `verbose` parameter from the URL string
    let verbose =
        if let Some(v) = params.as_ref().and_then(|p| p.get("verbose")) {
            match v.as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(v),
            }
        } else {
            Ok(false)
        };

    // Redirect `log` message to `console.log` and friends:
    eframe::WebLogger::init(if *verbose.as_ref().unwrap_or(&false) {
        // TODO this doesn't seem to work?
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    })
    .ok();

    info!("starting...");
    if let Err(e) = verbose {
        warn!(
            "invalid value for 'verbose': {e:?} (expected 'true' or 'false')"
        );
    }

    let example = params.and_then(|p| p.get("example"));
    wasm_bindgen_futures::spawn_local(async move {
        let canvas = get_canvas();
        let mut web_options = eframe::WebOptions::default();
        web_options.wgpu_options.wgpu_setup = match wgpu_setup().await {
            Ok(w) => w.into(),
            Err(e) => {
                let p = document.get_element_by_id("wgpu-error").unwrap();
                p.set_text_content(Some(&format!(
                    "{}",
                    anyhow::Error::from(e),
                )));
                let div = document.get_element_by_id("wgpu-fail").unwrap();
                div.remove_attribute("hidden").unwrap();
                canvas.remove();
                panic!("wgpu initialization failed");
            }
        };

        // Add a custom panic handler for subsequent panics
        std::panic::set_hook(Box::new(custom_panic_handler));

        eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| {
                    let (mut app, mut notify_rx) = App::new(cc, false);
                    if let Some(example) = example {
                        if !app.load_example(&example) {
                            warn!("failed to load example '{example}'");
                        }
                    }

                    // Spawn a worker task to trigger repaints,
                    // per egui#4368 and egui#4405
                    let ctx = cc.egui_ctx.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        while let Some(()) = notify_rx.recv().await {
                            ctx.request_repaint();
                        }
                        info!("repaint notification task is stopping");
                    });

                    Ok(Box::new(app))
                }),
            )
            .await
            .expect("failed to start eframe");
    });
}

impl App {
    pub(crate) fn platform_update_title(&self, _ctx: &egui::Context) {
        // no-op on the web backend
    }

    pub(crate) fn platform_save(&mut self) {
        self.on_save_local();
    }

    pub(crate) fn platform_save_as(&mut self) {
        self.on_save_as_local();
    }

    pub(crate) fn platform_open(&mut self) {
        if self.platform.dialogs.send(DialogRequest::Open).is_ok() {
            self.modal = Some(Modal::WaitForLoad);
        } else {
            error!("could not send Open to dialog thread");
        }
    }

    pub(crate) fn platform_select_download(
        &self,
        ext: &str,
    ) -> Option<ExportTarget> {
        if let Some(name) = &self.meta.name {
            Some(ExportTarget(format!("{name}.{ext}")))
        } else {
            Some(ExportTarget(format!("halfspace_export.{ext}")))
        }
    }
}

pub struct Data {
    /// Dialogs are handled in a separate task
    dialogs: tokio::sync::mpsc::UnboundedSender<DialogRequest>,
}

impl Data {
    /// Prefix to namespace file storage keys
    const FILE_PREFIX: &str = "vfs:";

    pub(crate) fn new(queue: MessageSender) -> Data {
        let (dialog_tx, dialog_rx) = tokio::sync::mpsc::unbounded_channel();
        wasm_bindgen_futures::spawn_local(dialog_worker(dialog_rx, queue));
        Data { dialogs: dialog_tx }
    }

    /// List all "files" in localStorage (keys starting with `vfs:`)
    pub(crate) fn list_local_storage(&self) -> Vec<String> {
        let storage = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .expect("localStorage not available");

        let mut result = Vec::new();
        let len = storage.length().unwrap_or(0);

        for i in 0..len {
            if let Some(key) = storage.key(i).unwrap_or(None) {
                if let Some(stripped) = key.strip_prefix(Self::FILE_PREFIX) {
                    result.push(stripped.to_string());
                }
            }
        }

        result
    }

    /// Write a file (string content) to a given path
    pub(crate) fn save_to_local_storage(&self, path: &str, contents: &str) {
        let storage = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .expect("localStorage not available");

        storage
            .set_item(&format!("{}{path}", Self::FILE_PREFIX), contents)
            .expect("failed to write to localStorage");
    }

    /// Read a file from a given path
    pub(crate) fn read_from_local_storage(&self, path: &str) -> String {
        let storage = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .expect("localStorage not available");

        storage
            .get_item(&format!("{}{path}", Self::FILE_PREFIX))
            .unwrap()
            .unwrap()
    }

    pub(crate) fn download_file(
        &self,
        filename: &str,
        data: &[u8],
    ) -> Option<Modal> {
        match Self::download_file_inner(filename, data) {
            Ok(()) => None,
            Err(e) => Some(Modal::Error {
                title: "Download failed".to_owned(),
                message: format!("{e:?}"),
            }),
        }
    }

    /// Downloads the given file
    pub fn download_file_inner(
        filename: &str,
        data: &[u8],
    ) -> Result<(), JsValue> {
        let uint8_array =
            js_sys::Uint8Array::new_with_length(data.len() as u32);
        uint8_array.copy_from(data);

        let array = js_sys::Array::new();
        array.push(&uint8_array);

        let blob_options = web_sys::BlobPropertyBag::new();
        blob_options.set_type("text/plain");

        // Create and return the Blob
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(
            &array,
            &blob_options,
        )?;

        // Create an object URL
        let url = web_sys::Url::create_object_url_with_blob(&blob)?;

        // Save the file
        Self::download_blob(filename, &url)?;

        // Clean up the URL
        web_sys::Url::revoke_object_url(&url)?;

        Ok(())
    }

    fn download_blob(file_name: &str, url: &str) -> Result<(), JsValue> {
        let document = web_sys::window().unwrap().document().unwrap();

        // Create the anchor element
        let a = document
            .create_element("a")?
            .dyn_into::<web_sys::HtmlAnchorElement>()?;
        a.set_href(url);
        a.set_download(file_name);
        a.set_attribute("style", "display: none")?;

        // Append to body and trigger click
        document.body().unwrap().append_child(&a)?;
        a.click();
        a.remove();

        Ok(())
    }
}

/// Platform-specific export target (downloads to a file)
#[derive(Debug)]
pub struct ExportTarget(String);

impl ExportTarget {
    pub fn save(&self, data: &[u8]) -> Result<(), std::io::Error> {
        Data::download_file_inner(&self.0, data)
            .map_err(|e| std::io::Error::other(format!("{e:?}")))
    }
}

pub enum DialogRequest {
    Open,
}

pub(crate) async fn dialog_worker(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<DialogRequest>,
    tx: MessageSender,
) {
    while let Some(m) = rx.recv().await {
        let r = match m {
            DialogRequest::Open => {
                if let Some(f) = rfd::AsyncFileDialog::new()
                    .add_filter("halfspace", &["half"])
                    .pick_file()
                    .await
                {
                    let data = f.read().await;
                    match std::str::from_utf8(&data)
                        .map_err(state::ReadError::NotUtf8)
                        .and_then(AppState::deserialize)
                    {
                        Ok(state) => Message::Loaded { state, path: None },
                        Err(e) => Message::LoadFailed {
                            title: "Open error".to_owned(),
                            message: format!("{:#}", anyhow::Error::from(e)),
                        },
                    }
                } else {
                    Message::CancelLoad
                }
            }
        };
        tx.send(r);
    }
    info!("dialog task is exiting");
}
