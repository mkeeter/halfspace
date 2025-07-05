use crate::{state, wgpu_setup, App};
use log::{info, warn};

pub fn run(target: Option<std::path::PathBuf>) -> Result<(), eframe::Error> {
    let mut native_options = eframe::NativeOptions::default();
    native_options.wgpu_options.wgpu_setup =
        pollster::block_on(wgpu_setup()).into();

    eframe::run_native(
        "halfspace",
        native_options,
        Box::new(|cc| {
            let (mut app, mut notify_rx) = App::new(cc);
            let ctx = cc.egui_ctx.clone();

            // Worker thread to request repaints based on notifications
            std::thread::spawn(move || {
                while let Some(()) = notify_rx.blocking_recv() {
                    ctx.request_repaint();
                }
                info!("repaint notification thread is stopping");
            });
            if let Some(filename) = target {
                match App::load_from_file(&filename) {
                    Ok(state) => {
                        info!("restoring state from file");
                        app.file = Some(filename);
                        app.load_from_state(state);
                        app.start_world_rebuild();
                    }
                    Err(state::ReadError::IoError(e))
                        if e.kind() == std::io::ErrorKind::NotFound =>
                    {
                        // We can specify a filename to create
                        warn!("file {filename:?} is not yet present");
                        app.file = Some(filename);
                    }
                    Err(e) => return Err(e.into()),
                };
            }
            Ok(Box::new(app))
        }),
    )
}

impl App {
    pub(crate) fn save(&mut self) {
        if let Some(f) = self.file.take() {
            self.write_to_file(&f).unwrap();
            self.file = Some(f);
        } else {
            let filename = rfd::FileDialog::new()
                .add_filter("halfspace", &["half"])
                .save_file();
            if let Some(filename) = filename {
                if self.write_to_file(&filename).is_ok() {
                    self.file = Some(filename);
                } else {
                    panic!("could not create file");
                }
            } else {
                warn!("file save cancelled due to empty selection");
            }
        }
    }

    pub(crate) fn save_as(&mut self) {
        let filename = rfd::FileDialog::new()
            .add_filter("halfspace", &["half"])
            .save_file();
        if let Some(filename) = filename {
            if self.write_to_file(&filename).is_ok() {
                self.file = Some(filename);
            } else {
                panic!("could not create file");
            }
        } else {
            warn!("file save cancelled due to empty selection");
        }
    }

    pub(crate) fn on_open(&mut self, ctx: &egui::Context) {
        let filename = rfd::FileDialog::new()
            .add_filter("halfspace", &["half"])
            .pick_file();
        if let Some(filename) = filename {
            if let Ok(state) = Self::load_from_file(&filename) {
                self.file = Some(filename);
                self.load_from_state(state);
                ctx.request_repaint();
            } else {
                panic!("could not load file");
            }
        }
    }
}
