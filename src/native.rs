use crate::{
    dialog_worker, state, wgpu_setup, App, AppState, Dialog, DialogRequest,
    Modal, NextAction,
};
use log::{error, info, warn};
use std::io::{Read, Write};

use clap::Parser;

/// An experimental CAD tool
#[derive(clap::Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Show verbose logging
    #[clap(short, long)]
    verbose: bool,

    /// Enable debug menu items
    #[clap(short, long)]
    debug: bool,

    /// Example to load
    #[clap(long, conflicts_with = "target")]
    example: Option<String>,

    /// File to edit (created if not present)
    target: Option<std::path::PathBuf>,
}

pub fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(if args.verbose {
            "halfspace=trace"
        } else {
            "halfspace=info"
        }),
    )
    .init();

    let mut native_options = eframe::NativeOptions::default();
    native_options.wgpu_options.wgpu_setup =
        pollster::block_on(wgpu_setup())?.into();

    eframe::run_native(
        "halfspace",
        native_options,
        Box::new(|cc| {
            let (dialog_tx, dialog_rx) = tokio::sync::mpsc::unbounded_channel();
            let (mut app, mut notify_rx) = App::new(cc, dialog_tx, args.debug);
            if let Some(example) = args.example {
                if !app.load_example(&format!("{example}.half")) {
                    warn!("could not find example '{example}'");
                }
            }

            let ctx = cc.egui_ctx.clone();

            let queue = app.rx.sender();
            std::thread::spawn(move || {
                pollster::block_on(dialog_worker(dialog_rx, queue))
            });

            // Worker thread to request repaints based on notifications
            std::thread::spawn(move || {
                while let Some(()) = notify_rx.blocking_recv() {
                    ctx.request_repaint();
                }
                info!("repaint notification thread is stopping");
            });
            if let Some(filename) = args.target {
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
                        info!("file {filename:?} is not yet present; treating it as empty");
                        app.file = Some(filename);
                    }
                    Err(e) => return Err(e.into()),
                };
            }
            Ok(Box::new(app))
        }),
    )?;

    Ok(())
}

impl App {
    fn load_from_file(
        filename: &std::path::Path,
    ) -> Result<AppState, state::ReadError> {
        info!("loading {filename:?}");
        let mut f = std::fs::File::options().read(true).open(filename)?;
        let mut data = vec![];
        f.read_to_end(&mut data)?;
        let s = std::str::from_utf8(&data)?;
        AppState::deserialize(s)
    }

    pub(crate) fn update_title(&mut self, ctx: &egui::Context) {
        let marker = if self.undo.is_saved() { "" } else { "*" };
        let title = if let Some(f) = &self.file {
            let f = f
                .file_name()
                .map(|s| s.to_string_lossy())
                .unwrap_or_else(|| "[no file name]".to_owned().into());
            format!("{f}{marker}")
        } else {
            format!("[untitled]{marker}")
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    pub(crate) fn on_save(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring save while modal is active");
        } else if self.file.is_some() {
            let f = self.file.take().unwrap();
            self.write_to_file(&f).unwrap();
            self.file = Some(f);
        } else {
            self.on_save_as();
        }
    }

    pub(crate) fn on_save_as(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring save as while modal is active");
        } else {
            let state = self.get_state();
            if self.dialogs.send(DialogRequest::SaveAs { state }).is_ok() {
                self.modal = Some(Modal::Dialog(Dialog::SaveAs));
            } else {
                error!("could not send SaveAs to dialog thread");
            }
        }
    }

    pub(crate) fn on_open(&mut self) {
        if self.modal.is_some() {
            warn!("cannot execute open with active modal");
        } else if self.undo.is_saved() {
            if self.dialogs.send(DialogRequest::Open).is_ok() {
                self.modal = Some(Modal::Dialog(Dialog::Open));
            } else {
                error!("could not send Open to dialog thread");
            }
        } else {
            self.modal = Some(Modal::Unsaved(NextAction::Open));
        }
    }

    pub(crate) fn on_upload(&mut self) {
        panic!("on_upload should not be called natively")
    }

    /// Writes to the given file and marks the current state as saved
    pub(crate) fn write_to_file(
        &mut self,
        filename: &std::path::Path,
    ) -> std::io::Result<()> {
        info!("writing to {filename:?}");
        let mut f = std::fs::File::options()
            .create(true)
            .truncate(true)
            .write(true)
            .open(filename)?;
        let state = self.get_state();
        f.write_all(state.serialize().as_bytes())?;
        f.flush()?;
        self.undo.mark_saved(state.world);
        Ok(())
    }
}

pub(crate) fn download_file(filename: &str, _text: &str) -> Option<Modal> {
    Some(Modal::Error {
        title: "Download failed".to_owned(),
        message: format!(
            "Downloading to {filename} isn't \
            implemented in the native platform"
        ),
    })
}

const LOCAL_STORAGE: &str = ".localdb";

pub(crate) fn list_local_storage() -> Vec<String> {
    let s = std::fs::read_to_string(LOCAL_STORAGE)
        .unwrap_or_else(|_| String::new());
    s.trim()
        .lines()
        .map(|line| line.split_once('|').unwrap().0.to_owned())
        .collect()
}

pub(crate) fn save_to_local_storage(path: &str, contents: &str) {
    let prev = std::fs::read_to_string(LOCAL_STORAGE)
        .unwrap_or_else(|_| String::new());
    let mut out = String::new();
    for line in prev.lines() {
        let (name, rest) = line.split_once('|').unwrap();
        if name != path {
            out += &format!("{name}|{rest}\n");
        }
    }
    let raw_contents = serde_json::to_string(&contents).unwrap();
    out += &format!("{path}|{raw_contents}\n");
    std::fs::write(LOCAL_STORAGE, out).unwrap();
}

pub(crate) fn read_from_local_storage(path: &str) -> String {
    let data = std::fs::read_to_string(LOCAL_STORAGE)
        .unwrap_or_else(|_| String::new());
    for line in data.lines() {
        let (name, rest) = line.split_once('|').unwrap();
        if name == path {
            return serde_json::from_str(rest).unwrap();
        }
    }
    panic!("file {path} not found");
}
