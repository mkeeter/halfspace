use crate::{App, AppState, Message, MessageSender, Modal, state, wgpu_setup};
use log::{info, warn};
use std::io::{Read, Write};

use clap::Parser;

/// No platform-specific data
#[derive(Default)]
pub struct Data;

impl Data {
    const LOCAL_STORAGE: &str = ".localdb";

    pub(crate) fn new(_queue: MessageSender) -> Data {
        Data
    }

    pub(crate) fn list_local_storage(&self) -> Vec<String> {
        let s = std::fs::read_to_string(Self::LOCAL_STORAGE)
            .unwrap_or_else(|_| String::new());
        s.trim()
            .lines()
            .map(|line| line.split_once('|').unwrap().0.to_owned())
            .collect()
    }

    pub(crate) fn save_to_local_storage(&self, path: &str, contents: &str) {
        let prev = std::fs::read_to_string(Self::LOCAL_STORAGE)
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
        std::fs::write(Self::LOCAL_STORAGE, out).unwrap();
    }

    pub(crate) fn read_from_local_storage(&self, path: &str) -> String {
        let data = std::fs::read_to_string(Self::LOCAL_STORAGE)
            .unwrap_or_else(|_| String::new());
        for line in data.lines() {
            let (name, rest) = line.split_once('|').unwrap();
            if name == path {
                return serde_json::from_str(rest).unwrap();
            }
        }
        panic!("file {path} not found");
    }

    pub(crate) fn download_file(
        &self,
        filename: &str,
        _data: &[u8],
    ) -> Option<Modal> {
        Some(Modal::Error {
            title: "Download failed".to_owned(),
            message: format!(
                "Downloading to {filename} isn't \
                implemented in the native platform"
            ),
        })
    }
}

/// Platform-specific export target
#[derive(Debug)]
pub struct ExportTarget(std::path::PathBuf);

impl ExportTarget {
    pub fn save(&self, data: &[u8]) -> Result<(), std::io::Error> {
        std::fs::File::create(&self.0)?.write_all(data)
    }
}

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
            let (mut app, notify_rx) = App::new(cc, args.debug);
            if let Some(example) = args.example
                && !app.load_example(&format!("{example}.half"))
            {
                warn!("could not find example '{example}'");
            }

            // Worker thread to request repaints based on notifications
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                while let Ok(()) = notify_rx.recv() {
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
                        info!(
                            "file {filename:?} is not yet present; treating it as empty"
                        );
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
    pub(crate) fn platform_update_title(&self, ctx: &egui::Context) {
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

    pub(crate) fn platform_save(&mut self) {
        if let Some(f) = self.file.take() {
            self.write_to_file(&f).unwrap();
            self.file = Some(f);
        } else {
            self.platform_save_as();
        }
    }

    pub(crate) fn platform_save_as(&mut self) {
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

    pub(crate) fn platform_open(&mut self) {
        assert!(self.modal.is_none());
        self.modal = Some(Modal::WaitForLoad);

        let filename = rfd::FileDialog::new()
            .add_filter("halfspace", &["half"])
            .pick_file();
        if let Some(filename) = filename {
            let m = match Self::load_from_file(&filename) {
                Ok(state) => Message::Loaded {
                    state,
                    path: Some(filename),
                },
                Err(e) => Message::LoadFailed {
                    title: "Load failed".to_owned(),
                    message: format!("{:#}", anyhow::Error::from(e)),
                },
            };
            self.rx.sender().send(m);
        } else {
            self.rx.sender().send(Message::CancelLoad);
        }
    }

    pub(crate) fn platform_select_download(
        &self,
        ext: &str,
    ) -> Option<ExportTarget> {
        rfd::FileDialog::new()
            .add_filter("STL", &[ext])
            .save_file()
            .map(ExportTarget)
    }
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

    /// Writes to the given file and marks the current state as saved
    fn write_to_file(
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
