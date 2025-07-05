use crate::{wgpu_setup, App};
use log::{info, warn};
use wasm_bindgen::prelude::*;

/// Re-export init_thread_pool to be called on the web
pub use wasm_bindgen_rayon::init_thread_pool;

#[wasm_bindgen]
pub fn run() {
    use eframe::wasm_bindgen::JsCast as _;

    // Redirect `log` message to `console.log` and friends:
    eframe::WebLogger::init(log::LevelFilter::Debug).ok();
    info!("starting...");

    let mut web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .expect("No window")
            .document()
            .expect("No document");

        let canvas = document
            .get_element_by_id("the_canvas_id")
            .expect("Failed to find the_canvas_id")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("the_canvas_id was not a HtmlCanvasElement");

        web_options.wgpu_options.wgpu_setup = wgpu_setup().await.into();

        eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| {
                    let (app, mut notify_rx) = App::new(cc);

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

// TODO: `rfd` theoretically supports WebAssembly, although it's async-only
impl App {
    pub(crate) fn save(&mut self) {
        warn!("cannot save in webassembly");
    }
    pub(crate) fn save_as(&mut self) {
        warn!("cannot save in webassembly");
    }
    pub(crate) fn on_open(&mut self, _ctx: &egui::Context) {
        warn!("cannot open in webassembly");
    }
}
