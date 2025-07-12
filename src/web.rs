use crate::{state, wgpu_setup, App, AppState, Message, MessageSender, Modal};
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
        let canvas = document
            .get_element_by_id("the_canvas_id")
            .expect("Failed to find the_canvas_id")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("the_canvas_id was not a HtmlCanvasElement");

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
                panic!();
            }
        };

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
        text: &str,
    ) -> Option<Modal> {
        match Self::download_file_inner(filename, text) {
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
        text: &str,
    ) -> Result<(), JsValue> {
        // Create a Blob from the text
        let blob_parts = js_sys::Array::new();
        blob_parts.push(&JsValue::from_str(text));

        let blob_options = web_sys::BlobPropertyBag::new();
        blob_options.set_type("text/plain");

        let blob = web_sys::Blob::new_with_str_sequence_and_options(
            &blob_parts,
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
