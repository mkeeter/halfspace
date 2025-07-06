use crate::{dialog_worker, wgpu_setup, App};
use log::{info, warn};
use wasm_bindgen::prelude::*;

/// Re-export init_thread_pool to be called on the web
pub use wasm_bindgen_rayon::init_thread_pool;

// YOLO zone
unsafe impl Sync for crate::painters::WgpuResources {}
unsafe impl Send for crate::painters::WgpuResources {}
unsafe impl Send for crate::WgpuError {}
unsafe impl Sync for crate::WgpuError {}

#[wasm_bindgen]
pub fn run() {
    use eframe::wasm_bindgen::JsCast as _;

    let window = web_sys::window().expect("No window");
    let document = window.document().expect("No document");
    let location = window.location();

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
                let p = document.create_element("p").unwrap();
                p.set_text_content(Some(&format!(
                    "WebGPU is not supported on this browser: {}",
                    anyhow::Error::from(e),
                )));
                let body =
                    document.body().expect("document should have a body");
                body.append_child(&p).unwrap();

                let p = document.create_element("p").unwrap();
                let text = document.create_text_node("Try ");
                p.append_child(&text).unwrap();
                let a = document
                    .create_element("a")
                    .unwrap()
                    .dyn_into::<web_sys::HtmlAnchorElement>()
                    .unwrap();
                a.set_href("https://www.google.com/chrome");
                a.set_text_content(Some("Google Chrome"));
                p.append_child(&a).unwrap();

                let comma = document.create_text_node(", ");
                p.append_child(&comma).unwrap();
                let a = document
                    .create_element("a")
                    .unwrap()
                    .dyn_into::<web_sys::HtmlAnchorElement>()
                    .unwrap();
                a.set_href("https://developer.apple.com/documentation/safari-release-notes/safari-26-release-notes");
                a.set_text_content(Some("Safari 26 (beta)"));
                p.append_child(&a).unwrap();
                let comma = document.create_text_node(", or ");
                p.append_child(&comma).unwrap();
                let a = document
                    .create_element("a")
                    .unwrap()
                    .dyn_into::<web_sys::HtmlAnchorElement>()
                    .unwrap();
                a.set_href(
                    "https://www.mozilla.org/en-US/firefox/channel/desktop/",
                );
                a.set_text_content(Some("Firefox Nightly"));
                p.append_child(&a).unwrap();
                let comma = document.create_text_node(".");
                p.append_child(&comma).unwrap();

                body.append_child(&p).unwrap();
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
                    let (mut app, mut notify_rx) = App::new(cc, dialog_tx);
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
