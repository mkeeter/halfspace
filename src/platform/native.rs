use crate::{
    App, AppState, Message, MessageSender, Modal,
    platform::{self, Platform, PlatformData},
    state, wgpu_setup,
};
use log::{info, warn};
use std::{
    io::{Read, Write},
    path::PathBuf,
};

use clap::Parser;

struct NativePlatform;

impl Platform for NativePlatform {
    type Data = Data;
    type ExportTarget = ExportTarget;
    type Notify = Notify;
}

pub struct Data {
    queue: MessageSender<Notify>,
    ctx: egui::Context,
}

impl Data {
    const LOCAL_STORAGE: &str = ".localdb";
}

impl PlatformData<NativePlatform> for Data {
    fn new(ctx: &egui::Context, queue: MessageSender<Notify>) -> Self {
        Self {
            queue,
            ctx: ctx.clone(),
        }
    }

    fn list_local_storage(&self) -> Vec<String> {
        let s = std::fs::read_to_string(Self::LOCAL_STORAGE)
            .unwrap_or_else(|_| String::new());
        s.trim()
            .lines()
            .map(|line| line.split_once('|').unwrap().0.to_owned())
            .collect()
    }

    fn save_to_local_storage(&self, path: &str, contents: &str) {
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

    fn read_from_local_storage(&self, path: &str) -> String {
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

    fn download_file(
        &self,
        filename: &str,
        _data: &[u8],
    ) -> Option<Modal<ExportTarget>> {
        Some(Modal::Error {
            title: "Download failed".to_owned(),
            message: format!(
                "Downloading to {filename} isn't \
                implemented in the native platform"
            ),
        })
    }

    fn open(&self) -> Option<Modal<ExportTarget>> {
        let filename = rfd::FileDialog::new()
            .add_filter("halfspace", &["half"])
            .pick_file();
        if let Some(filename) = filename {
            let m = match load_from_file(&filename) {
                Ok(state) => Message::Loaded {
                    state,
                    path: Some(filename),
                },
                Err(e) => Message::LoadFailed {
                    title: "Load failed".to_owned(),
                    message: format!("{:#}", anyhow::Error::from(e)),
                },
            };
            self.queue.send(m);
        } else {
            self.queue.send(Message::CancelLoad);
        }
        Some(Modal::WaitForLoad)
    }

    fn can_save(&self) -> bool {
        true
    }

    fn save(
        &self,
        state: &AppState,
        f: &std::path::Path,
    ) -> std::io::Result<()> {
        write_to_file(state, f)
    }

    fn save_as(&self, state: &AppState) -> std::io::Result<Option<PathBuf>> {
        let filename = rfd::FileDialog::new()
            .add_filter("halfspace", &["half"])
            .save_file();
        if let Some(filename) = filename {
            write_to_file(state, &filename)?;
            Ok(Some(filename))
        } else {
            warn!("file save cancelled due to empty selection");
            Ok(None)
        }
    }

    fn export_name(
        &self,
        _name: Option<&str>,
        dialog_name: &str,
        extension: &str,
    ) -> Option<ExportTarget> {
        rfd::FileDialog::new()
            .add_filter(dialog_name, &[extension])
            .save_file()
            .map(ExportTarget)
    }

    fn update_title(&self, title: &str) {
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Title(title.to_owned()));
    }
}

/// Platform-specific export target
#[derive(Debug)]
pub struct ExportTarget(std::path::PathBuf);

impl platform::PlatformExport for ExportTarget {
    fn save(&self, data: &[u8]) -> Result<(), std::io::Error> {
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

#[derive(Clone)]
pub struct Notify(egui::Context);

impl platform::Notify for Notify {
    type Err = std::convert::Infallible;
    fn wake(&self) -> Result<(), std::convert::Infallible> {
        self.0.request_repaint();
        Ok(())
    }
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
            let ctx = cc.egui_ctx.clone();
            let notify = Notify(ctx);
            let mut app = App::<NativePlatform>::new(cc, notify, args.debug);
            if let Some(example) = args.example
                && !app.load_example(&format!("{example}.half"))
            {
                warn!("could not find example '{example}'");
            }

            if let Some(filename) = args.target {
                match load_from_file(&filename) {
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

impl App<NativePlatform> {}

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

/// Writes to the given file
fn write_to_file(
    state: &AppState,
    filename: &std::path::Path,
) -> std::io::Result<()> {
    info!("writing to {filename:?}");
    let mut f = std::fs::File::options()
        .create(true)
        .truncate(true)
        .write(true)
        .open(filename)?;
    f.write_all(state.serialize().as_bytes())?;
    f.flush()?;
    Ok(())
}
