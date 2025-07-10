use crate::{dialog_worker, state, wgpu_setup, App, AppState, Modal};
use log::{info, warn};
use std::io::Read;

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
