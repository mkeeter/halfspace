use clap::Parser;
use egui_dnd::dnd;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, Write};
use zerocopy::IntoBytes;

mod draw;
mod gui;
mod view;
mod world;

use world::{BlockIndex, World};

/// An experimental CAD tool
#[derive(clap::Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// File to edit (created if not present)
    target: Option<std::path::PathBuf>,
}

pub fn main() -> Result<(), eframe::Error> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();
    let args = Args::parse();

    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "halfspace",
        native_options,
        Box::new(|cc| {
            let mut app = App::new(cc);
            if let Some(f) = args.target {
                let mut f = create_rw_file(f)?;
                f.seek(std::io::SeekFrom::End(0))?;
                let file_length = f.stream_position()?;
                app.file = Some(f);
                if file_length != 0 {
                    app.load_from_file()?;
                    app.start_world_rebuild(&cc.egui_ctx);
                }
            }
            Ok(Box::new(app))
        }),
    )
}

enum Message {
    RebuildWorld {
        world: World,
        generation: u64,
    },
    RenderView {
        block: BlockIndex,
        generation: u64,
        level: usize,
        settings: view::RenderSettings,
        start_time: std::time::Instant,
        data: Vec<[u8; 4]>,
    },
}

struct App {
    data: World,
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,

    file: Option<std::fs::File>,

    tree: egui_dock::DockState<gui::Tab>,
    syntax: egui_extras::syntax_highlighting::SyntectSettings,
    views: HashMap<BlockIndex, view::ViewData>,

    rx: std::sync::mpsc::Receiver<Message>,
    tx: std::sync::mpsc::Sender<Message>,
}

/// Serialization-friendly state
#[derive(Clone, Serialize, Deserialize)]
struct AppState {
    data: World,
    tree: egui_dock::DockState<gui::Tab>,
    views: HashMap<BlockIndex, view::ViewMode>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Install custom render pipelines
        let wgpu_state = cc.wgpu_render_state.as_ref().unwrap();
        draw::WgpuResources::install(wgpu_state);

        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "inconsolata".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(INCONSOLATA)),
        );
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "inconsolata".to_owned());

        let (ps, _) = bincode::serde::decode_from_slice(
            SYNTAX,
            bincode::config::standard(),
        )
        .unwrap();
        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let syntax =
            egui_extras::syntax_highlighting::SyntectSettings { ps, ts };

        cc.egui_ctx.set_fonts(fonts);
        cc.egui_ctx.all_styles_mut(|style| {
            style.interaction.selectable_labels = false;
            style.text_styles.insert(
                egui::TextStyle::Heading,
                egui::FontId::new(16.0, egui::FontFamily::Proportional),
            );
            style.text_styles.insert(
                egui::TextStyle::Body,
                egui::FontId::new(16.0, egui::FontFamily::Proportional),
            );
            style.text_styles.insert(
                egui::TextStyle::Button,
                egui::FontId::new(16.0, egui::FontFamily::Proportional),
            );
            style.text_styles.insert(
                egui::TextStyle::Monospace,
                egui::FontId::new(16.0, egui::FontFamily::Proportional),
            );
            style.interaction.tooltip_delay = 0.0;
            style.interaction.show_tooltips_only_when_still = false;
        });

        // Customize egui here with cc.egui_ctx.set_fonts and cc.egui_ctx.set_visuals.
        // Restore app state using cc.storage (requires the "persistence" feature).
        // Use the cc.gl (a glow::Context) to create graphics shaders and buffers that you can use
        // for e.g. egui::PaintCallback.
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            data: World::new(),
            tree: egui_dock::DockState::new(vec![]),
            file: None,
            syntax,
            views: HashMap::new(),
            generation: std::sync::Arc::new(0.into()),
            tx,
            rx,
        }
    }

    /// Returns a serializable state
    fn state(&self) -> AppState {
        AppState {
            data: self.data.clone(),
            tree: self.tree.clone(),
            views: self
                .views
                .iter()
                .map(|(k, v)| (*k, v.canvas.into()))
                .collect(),
        }
    }

    /// Writes to the file in `self.file`
    ///
    /// # Panics
    /// If `self.file` is `None`
    fn write_to_file(&mut self) -> std::io::Result<()> {
        let state = self.state();
        let cfg = bincode::config::standard();
        let state_data = bincode::serde::encode_to_vec(&state, cfg).unwrap();
        let f = self.file.as_mut().unwrap();
        f.rewind()?;
        f.set_len(0)?;
        f.write_all(FILE_MAGIC.as_bytes())?;
        f.write_all(FILE_VERSION.as_bytes())?;
        f.write_all((state_data.len() as u64).as_bytes())?;
        f.write_all(&state_data)?;
        f.flush()?;
        Ok(())
    }

    /// Loads from the file in `self.file`
    ///
    /// # Panics
    /// If `self.file` is `None`
    fn load_from_file(&mut self) -> Result<(), ReadError> {
        let f = self.file.as_mut().unwrap();

        f.seek(std::io::SeekFrom::End(0))?;
        let file_length = f.stream_position()?;
        let Some(data_length) = file_length.checked_sub(HEADER_LENGTH as u64)
        else {
            return Err(ReadError::TooShort);
        };

        f.rewind()?;
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic)?;
        if magic != FILE_MAGIC {
            return Err(ReadError::BadMagic {
                expected: FILE_MAGIC,
                actual: magic,
            });
        }
        let mut file_version = 0u32;
        f.read_exact(file_version.as_mut_bytes())?;
        if file_version != FILE_VERSION {
            return Err(ReadError::BadVersion {
                expected: FILE_VERSION,
                actual: file_version,
            });
        }
        let mut expected_len = 0u64;
        f.read_exact(expected_len.as_mut_bytes())?;
        if expected_len != data_length {
            return Err(ReadError::BadFileSize {
                expected: expected_len,
                actual: data_length,
            });
        }
        if expected_len > 4 * 1024u64.pow(3) {
            return Err(ReadError::SuspiciouslyLarge(expected_len));
        }
        let mut data = vec![0u8; expected_len as usize];
        f.read_exact(&mut data)?;

        let cfg = bincode::config::standard();
        let state: AppState = bincode::serde::decode_from_slice(&data, cfg)
            .map_err(ReadError::DecodeError)?
            .0;

        self.data = state.data;
        self.tree = state.tree;
        self.views = state
            .views
            .into_iter()
            .map(|(k, v)| (k, view::ViewData::from(view::ViewCanvas::from(v))))
            .collect();
        self.generation
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.views = HashMap::new();
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx = tx; // use a new channel to orphan previous tasks
        self.rx = rx;

        Ok(())
    }

    fn start_world_rebuild(&self, ctx: &egui::Context) {
        // Send the world to a worker thread for re-evaluation
        let mut world = self.data.without_state();
        let ctx = ctx.clone();
        let tx = self.tx.clone();
        let generation = self
            .generation
            .fetch_add(1u64, std::sync::atomic::Ordering::Release)
            + 1;
        let gen_handle = self.generation.clone();
        rayon::spawn(move || {
            // The world may have moved on before this script
            // evaluation started; if so, then skip it entirely.
            let current_gen =
                gen_handle.load(std::sync::atomic::Ordering::Acquire);
            if current_gen == generation {
                world.rebuild();
                // Re-check generation before sending
                let current_gen =
                    gen_handle.load(std::sync::atomic::Ordering::Acquire);
                if current_gen == generation
                    && tx
                        .send(Message::RebuildWorld { generation, world })
                        .is_ok()
                {
                    ctx.request_repaint();
                }
            }
        })
    }
}

const FILE_VERSION: u32 = 1;
const FILE_MAGIC: [u8; 4] = *b"HALF";
const HEADER_LENGTH: usize = 4 + 4 + 8; // magic, version, length

#[derive(thiserror::Error, Debug)]
enum ReadError {
    #[error("file is too short to contain the mandatory header")]
    TooShort,
    #[error(
        "file has a bad magic header: expected {expected:?}, found {actual:?}"
    )]
    BadMagic { expected: [u8; 4], actual: [u8; 4] },
    #[error("file has a bad version: expected {expected}, found {actual}")]
    BadVersion { expected: u32, actual: u32 },
    #[error("file has a bad size: expected {expected}, found {actual}")]
    BadFileSize { expected: u64, actual: u64 },
    #[error("io error encountered when loading file")]
    IoError(#[from] std::io::Error),
    #[error("file is suspiciously large ({0} bytes)")]
    SuspiciouslyLarge(u64),
    #[error("bincode failed to parse the file")]
    DecodeError(bincode::error::DecodeError),
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let wgpu_state = frame.wgpu_render_state().unwrap();
        if let Some(r) = wgpu_state
            .renderer
            .write()
            .callback_resources
            .get_mut::<draw::WgpuResources>()
        {
            r.reset();
        }

        // Receive new data from the worker pool
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Message::RebuildWorld { generation, world } => {
                    if generation
                        == self
                            .generation
                            .load(std::sync::atomic::Ordering::Acquire)
                    {
                        self.data = world;
                    }
                }
                Message::RenderView {
                    block,
                    generation,
                    level,
                    settings,
                    data,
                    start_time,
                } => {
                    if let Some(e) = self.views.get_mut(&block) {
                        e.update(
                            generation,
                            level,
                            data,
                            settings,
                            start_time.elapsed(),
                        )
                    }
                }
            }
        }
        let mut out = AppResponse::empty();
        ctx.input_mut(|i| {
            if i.consume_shortcut(&egui::KeyboardShortcut {
                modifiers: egui::Modifiers::MAC_CMD,
                logical_key: egui::Key::Q,
            }) {
                out |= AppResponse::QUIT;
            }
            if i.consume_shortcut(&egui::KeyboardShortcut {
                modifiers: egui::Modifiers::MAC_CMD,
                logical_key: egui::Key::S,
            }) {
                out |= AppResponse::SAVE;
            }
        });

        egui::SidePanel::left("left_panel")
            .min_width(250.0)
            .show(ctx, |ui| {
                egui::menu::bar(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui.button("Save").clicked() {
                            out |= AppResponse::SAVE;
                        }
                        if ui.button("Quit").clicked() {
                            out |= AppResponse::QUIT;
                        }
                    });
                });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.left(ui) {
                        out |= AppResponse::WORLD_CHANGED;
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .inner_margin(0.)
                    .fill(egui::Color32::TRANSPARENT),
            )
            .show(ctx, |ui| {
                // Manually draw a backdrop; this will be covered by the
                // DockArea if there's anything being drawn
                let style = ui.style();
                let painter = ui.painter();
                let layout = painter.layout(
                    "nothing selected".to_owned(),
                    style.text_styles[&egui::TextStyle::Heading].clone(),
                    style.visuals.widgets.noninteractive.text_color(),
                    f32::INFINITY,
                );
                let rect = painter.clip_rect();
                let text_corner = rect.center() - layout.size() / 2.0;
                painter.rect_filled(rect, 0.0, style.visuals.panel_fill);
                painter.galley(text_corner, layout, egui::Color32::BLACK);

                let mut io_out = vec![];
                let mut bw = gui::WorldView {
                    world: &mut self.data,
                    syntax: &self.syntax,
                    views: &mut self.views,
                    tx: &self.tx,
                    out: &mut io_out,
                };
                egui_dock::DockArea::new(&mut self.tree)
                    .style(egui_dock::Style::from_egui(ctx.style().as_ref()))
                    .show_leaf_collapse_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .show_inside(ui, &mut bw);
                for (block, flags) in io_out {
                    for f in flags.iter() {
                        match f {
                            ViewResponse::FOCUS_ERR => {
                                let tab = gui::Tab::script(block);
                                let tab_location = self.tree.find_tab(&tab);
                                if let Some(tab_location) = tab_location {
                                    self.tree.set_active_tab(tab_location)
                                } else {
                                    self.tree.push_to_focused_leaf(tab);
                                }
                            }
                            ViewResponse::CHANGED => {
                                out |= AppResponse::WORLD_CHANGED;
                            }
                            ViewResponse::REDRAW => {
                                ui.ctx().request_repaint();
                            }
                            _ => panic!("invalid flag"),
                        }
                    }
                }
            });
        for f in out.iter() {
            match f {
                AppResponse::WORLD_CHANGED => {
                    self.start_world_rebuild(ctx);
                }
                AppResponse::QUIT => {
                    std::process::exit(0);
                }
                AppResponse::SAVE => {
                    if self.file.is_none() {
                        for i in 0..100 {
                            if let Ok(f) =
                                create_rw_file(format!("model_{i}.half"))
                            {
                                self.file = Some(f);
                                break;
                            }
                        }
                        if self.file.is_none() {
                            panic!("could not create file");
                        }
                    }
                    self.write_to_file().unwrap();
                }
                _ => panic!("invalid flag"),
            }
        }
    }
}

fn create_rw_file<P: AsRef<std::path::Path>>(
    p: P,
) -> std::io::Result<std::fs::File> {
    std::fs::File::options()
        .create(true)
        .append(true)
        .read(true)
        .open(p)
}

impl App {
    /// Draws the left side panel
    ///
    /// Returns `true` if anything changed
    #[must_use]
    fn left(&mut self, ui: &mut egui::Ui) -> bool {
        // Draw blocks
        let mut to_delete = HashSet::new();
        let mut changed = false;
        let last = self.data.order.last().cloned();
        // XXX there is a drag-and-drop implementation that's built into egui,
        // see `egui_demo_lib/src/demo/drag_and_drop.rs`
        dnd(ui, "dnd").show_vec(
            &mut self.data.order,
            |ui, index, handle, state| {
                let script_index = gui::Tab::script(*index);
                let view_index = gui::Tab::view(*index);
                let tab_location = self.tree.find_tab(&script_index);
                let mut view_location = self.tree.find_tab(&view_index);
                let block = self.data.blocks.get_mut(index).unwrap();

                // If we have an open view but block is (1) valid and (2) no
                // longer defines a view, then close the view.  We'll leave the
                // view open if the block isn't valid, to prevent views from
                // flicking in and out as a script is edited.
                let block_defines_view =
                    block.state.as_ref().is_some_and(|s| s.view.is_some());
                if let Some(v) = view_location {
                    if block.is_valid() && !block_defines_view {
                        self.tree.remove_tab(v);
                        view_location = None;
                    }
                }

                let flags = gui::BlockUiFlags {
                    is_last: Some(*index) == last,
                    is_open: tab_location.is_some(),
                    is_dragged: state.dragged,
                    is_view_open: view_location.map(|_| true).or(
                        if block_defines_view {
                            Some(false)
                        } else {
                            None
                        },
                    ),
                };
                let r = gui::draggable_block(ui, *index, block, flags, handle);
                if r.contains(BlockResponse::DELETE) {
                    to_delete.insert(*index);
                    if let Some(tab_location) = tab_location {
                        self.tree.remove_tab(tab_location).unwrap();
                    }
                    if let Some(view_location) = view_location {
                        self.tree.remove_tab(view_location).unwrap();
                    }
                }
                if r.contains(BlockResponse::TOGGLE_EDIT) {
                    if let Some(tab_location) = tab_location {
                        self.tree.remove_tab(tab_location).unwrap();
                    } else {
                        self.tree.push_to_focused_leaf(script_index);
                    }
                }
                if r.contains(BlockResponse::TOGGLE_VIEW) {
                    if let Some(view_location) = view_location {
                        self.tree.remove_tab(view_location).unwrap();
                    } else {
                        self.tree.push_to_focused_leaf(view_index);
                    }
                }
                if r.contains(BlockResponse::FOCUS_ERR) {
                    if let Some(tab_location) = tab_location {
                        self.tree.set_active_tab(tab_location)
                    } else {
                        self.tree.push_to_focused_leaf(script_index);
                    }
                }
                changed |= r.contains(BlockResponse::CHANGED);
            },
        );

        // Post-processing: edit blocks based on button presses
        changed |= self.data.retain(|index| !to_delete.contains(index));
        self.views.retain(|index, _| !to_delete.contains(index));

        // Draw the "new block" button below a separator
        if !self.data.is_empty() {
            ui.separator();
        }
        if ui.button(gui::NEW_BLOCK).clicked() {
            changed |= self.data.new_empty_block();
        }
        changed
    }
}

bitflags::bitflags! {
    /// Represents a set of flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[must_use]
    struct BlockResponse: u32 {
        /// Request to delete the block
        const DELETE        = (1 << 0);
        /// Request to toggle the edit window
        const TOGGLE_EDIT   = (1 << 1);
        /// Request to toggle the view window
        const TOGGLE_VIEW   = (1 << 2);
        /// Request to focus the edit window
        const FOCUS_ERR     = (1 << 3);
        /// The block has changed
        const CHANGED       = (1 << 4);
    }
}

bitflags::bitflags! {
    /// Represents a set of flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[must_use]
    struct ViewResponse: u32 {
        /// Request to focus the edit window
        const FOCUS_ERR     = (1 << 0);
        /// The block has changed
        const CHANGED       = (1 << 1);
        /// The UI should be repainted
        const REDRAW        = (1 << 2);
    }
}

bitflags::bitflags! {
    /// Flags representing changes in the `App`
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[must_use]
    struct AppResponse: u32 {
        /// Request to save the model
        const SAVE          = (1 << 0);
        /// Request to quit the application
        const QUIT          = (1 << 1);
        /// The world should be re-evaluate
        const WORLD_CHANGED = (1 << 2);
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Inconsolata with additional icons, included in the binary
const INCONSOLATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fonts/InconsolataNerdFontPropo-Regular.ttf"
));

/// `SyntextSet` for Rhai, generated by `build.rs` and serialized with bincode
const SYNTAX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/syntax.bin"));
