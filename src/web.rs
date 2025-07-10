use crate::{dialog_worker, wgpu_setup, App, Modal};
use log::{info, warn};
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
                    let (dialog_tx, dialog_rx) =
                        tokio::sync::mpsc::unbounded_channel();
                    let (mut app, mut notify_rx) =
                        App::new(cc, dialog_tx, false);
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

                    let queue = app.rx.sender();
                    wasm_bindgen_futures::spawn_local(dialog_worker(
                        dialog_rx, queue,
                    ));

                    Ok(Box::new(app))
                }),
            )
            .await
            .expect("failed to start eframe");
    });
}

impl App {
    pub(crate) fn update_title(&mut self, _ctx: &egui::Context) {
        // no-op on the web backend
    }
}

pub(crate) fn download_file(filename: &str, text: &str) -> Option<Modal> {
    match download_file_inner(filename, text) {
        Ok(()) => None,
        Err(j) => Some(Modal::Error {
            title: "Download failed".to_owned(),
            message: format!("{j:?}"),
        }),
    }
}

/// Downloads the given file
pub fn download_file_inner(filename: &str, text: &str) -> Result<(), JsValue> {
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
    download_blob(filename, &url)?;

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
