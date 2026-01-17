use eframe::egui_wgpu::wgpu;
use egui_dnd::dnd;
use log::{debug, error, info, warn};
use std::collections::{HashMap, HashSet};
use web_time::Instant;

mod export;
mod gui;
mod painters;
mod render;
mod state;
mod view;
mod world;

pub mod platform;

use state::{AppState, WorldState};
use world::{BlockIndex, World};

#[derive(thiserror::Error, Debug)]
pub enum WgpuError {
    #[error("could not get WebGPU adapter")]
    RequestAdapterError(#[from] wgpu::RequestAdapterError),

    #[error("missing feature `{0}`")]
    MissingFeature(&'static str),

    #[error("could not request device")]
    RequestDeviceError(#[from] wgpu::RequestDeviceError),
}

/// Manually open a WebGPU session with `float32-filterable`
pub async fn wgpu_setup() -> Result<egui_wgpu::WgpuSetupExisting, WgpuError> {
    let instance = wgpu::Instance::default();
    info!("calling wgpu_setup...");

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await?;

    let adapter_features = adapter.features();
    if !adapter_features.contains(wgpu::Features::FLOAT32_FILTERABLE) {
        return Err(WgpuError::MissingFeature("float32-filterable"));
    }

    let required_features = wgpu::Features::FLOAT32_FILTERABLE;

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("Device with float32-filterable"),
            required_features,
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        })
        .await?;

    Ok(egui_wgpu::WgpuSetupExisting {
        instance,
        adapter,
        device,
        queue,
    })
}

// Base-16 Eighties, approximately
pub mod color {
    pub const BASE0: egui::Color32 = egui::Color32::from_rgb(0x20, 0x20, 0x20);

    pub const BASE00: egui::Color32 = egui::Color32::from_rgb(0x2d, 0x2d, 0x2d);
    pub const BASE01: egui::Color32 = egui::Color32::from_rgb(0x39, 0x39, 0x39);
    pub const BASE02: egui::Color32 = egui::Color32::from_rgb(0x51, 0x51, 0x51);
    pub const BASE03: egui::Color32 = egui::Color32::from_rgb(0x74, 0x73, 0x69);
    pub const BASE04: egui::Color32 = egui::Color32::from_rgb(0xa0, 0x9f, 0x93);
    pub const BASE05: egui::Color32 = egui::Color32::from_rgb(0xd3, 0xd0, 0xc8);
    pub const BASE06: egui::Color32 = egui::Color32::from_rgb(0xe8, 0xe6, 0xdf);
    pub const BASE07: egui::Color32 = egui::Color32::from_rgb(0xf2, 0xf0, 0xec);

    pub const RED: egui::Color32 = egui::Color32::from_rgb(0xf2, 0x77, 0x7a);
    pub const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xf9, 0x91, 0x57);
    pub const YELLOW: egui::Color32 = egui::Color32::from_rgb(0xff, 0xcc, 0x66);
    pub const GREEN: egui::Color32 = egui::Color32::from_rgb(0x99, 0xcc, 0x99);
    pub const CYAN: egui::Color32 = egui::Color32::from_rgb(0x66, 0xcc, 0xcc);
    pub const BLUE: egui::Color32 = egui::Color32::from_rgb(0x66, 0x99, 0xcc);
    pub const PINK: egui::Color32 = egui::Color32::from_rgb(0xcc, 0x99, 0xcc);
    pub const BROWN: egui::Color32 = egui::Color32::from_rgb(0xd2, 0x7b, 0x53);

    pub const LIGHT_BLUE: egui::Color32 =
        egui::Color32::from_rgb(0xbb, 0xcc, 0xee);
    pub const DARK_BLUE: egui::Color32 =
        egui::Color32::from_rgb(0x33, 0x66, 0x99);
}

fn theme_visuals() -> egui::Visuals {
    use color::*;
    let base = egui::Visuals::dark();

    // Helper function to translate a color into our theme
    let c = |c: egui::Color32| {
        if c == egui::Color32::from_rgb(255, 143, 0) {
            ORANGE
        } else if c == egui::Color32::from_rgb(0, 92, 128) {
            DARK_BLUE
        } else if c == egui::Color32::from_rgb(90, 170, 255) {
            BLUE
        } else if c == egui::Color32::from_rgb(192, 222, 255) {
            LIGHT_BLUE
        } else if c == egui::Color32::from_rgb(255, 0, 0) {
            RED
        } else if c.r() == c.g() && c.g() == c.b() {
            // Linear mapping from dark to light, chosen somewhat arbitrarily
            const ARRAY: [egui::Color32; 2] = [BASE0, BASE07];
            let frac = (c.r() as f32 / 255.0) * (ARRAY.len() as f32 - 1.0);
            let i = frac as usize;
            if i as f32 == frac {
                ARRAY[i]
            } else {
                let f = frac - i as f32;
                let blend = |a, b| (a as f32 * (1.0 - f) + b as f32 * f) as u8;
                let a = ARRAY[i];
                let b = ARRAY[i + 1];
                egui::Color32::from_rgb(
                    blend(a.r(), b.r()),
                    blend(a.b(), b.b()),
                    blend(a.g(), b.g()),
                )
            }
        } else {
            warn!("unknown color {c:?}");
            c
        }
    };
    let s = |s: egui::Stroke| egui::Stroke {
        color: c(s.color),
        ..s
    };
    let t = |t: egui::style::TextCursorStyle| egui::style::TextCursorStyle {
        stroke: s(t.stroke),
        ..t
    };
    let w = |v: egui::style::WidgetVisuals| egui::style::WidgetVisuals {
        bg_fill: c(v.bg_fill),
        weak_bg_fill: c(v.weak_bg_fill),
        bg_stroke: s(v.bg_stroke),
        fg_stroke: s(v.fg_stroke),
        expansion: v.expansion,
        corner_radius: v.corner_radius,
    };

    egui::Visuals {
        dark_mode: base.dark_mode,
        override_text_color: Some(BASE07),
        selection: egui::style::Selection {
            bg_fill: c(base.selection.bg_fill),
            stroke: s(base.selection.stroke),
        },
        hyperlink_color: c(base.hyperlink_color),
        faint_bg_color: c(base.faint_bg_color),
        extreme_bg_color: c(base.extreme_bg_color),
        code_bg_color: c(base.code_bg_color),
        warn_fg_color: c(base.warn_fg_color),
        error_fg_color: c(base.error_fg_color),
        window_fill: c(base.window_fill),
        window_stroke: s(base.window_stroke),
        panel_fill: c(base.panel_fill),
        text_cursor: t(base.text_cursor),

        text_options: base.text_options,
        weak_text_alpha: base.weak_text_alpha,
        weak_text_color: base.weak_text_color.map(c),
        text_edit_bg_color: base.text_edit_bg_color.map(c),
        disabled_alpha: base.disabled_alpha,

        collapsing_header_frame: base.collapsing_header_frame,
        handle_shape: base.handle_shape,
        window_corner_radius: base.window_corner_radius,
        window_shadow: base.window_shadow,
        image_loading_spinners: base.image_loading_spinners,
        window_highlight_topmost: base.window_highlight_topmost,
        menu_corner_radius: base.menu_corner_radius,
        popup_shadow: base.popup_shadow,
        resize_corner_size: base.resize_corner_size,
        indent_has_left_vline: base.indent_has_left_vline,
        striped: base.striped,
        slider_trailing_fill: base.slider_trailing_fill,
        interact_cursor: base.interact_cursor,
        numeric_color_space: base.numeric_color_space,

        button_frame: base.button_frame,
        clip_rect_margin: base.clip_rect_margin,

        widgets: egui::style::Widgets {
            noninteractive: w(base.widgets.noninteractive),
            inactive: w(base.widgets.inactive),
            hovered: w(base.widgets.hovered),
            active: w(base.widgets.active),
            open: w(base.widgets.open),
        },
    }
}

#[allow(clippy::large_enum_variant)]
enum Message {
    RebuildWorld {
        world: World,
    },
    RenderView {
        block: BlockIndex,
        generation: u64,
        start_time: Instant,
        data: view::ViewImage,
    },
    Loaded {
        state: AppState,
        path: Option<std::path::PathBuf>,
    },
    LoadFailed {
        title: String,
        message: String,
    },
    CancelLoad,
    ExportComplete(Result<Vec<u8>, export::ExportError>),
}

/// Message sender for worker tasks
#[derive(Clone)]
struct MessageSenderInner {
    /// Main (blocking) queue for messages
    queue: std::sync::mpsc::Sender<(Message, Option<u64>)>,

    /// Optionally async queue, for `ctx` notifications
    notify: flume::Sender<()>,
}

/// Message sender tagged with a generation
///
/// This is used by long-running tasks which may be cancelled, e.g. evaluating a
/// script or rendering an image.  If we load a new file, we increment the
/// generation, which causes messages from older tasks to be discarded when
/// received.
#[derive(Clone)]
struct MessageGenSender {
    /// Inner queue
    inner: MessageSenderInner,

    /// Generation to be send with each message
    generation: u64,
}

/// Unconditional message sender
#[derive(Clone)]
struct MessageSender {
    /// Inner queue
    inner: MessageSenderInner,
}

struct MessageReceiver {
    receiver: std::sync::mpsc::Receiver<(Message, Option<u64>)>,
    generation: u64,

    sender: std::sync::mpsc::Sender<(Message, Option<u64>)>,
    notify: flume::Sender<()>,
}

impl MessageReceiver {
    fn new(notify: flume::Sender<()>) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self {
            sender,
            receiver,
            notify,
            generation: 0,
        }
    }
    fn try_recv(&self) -> Option<Message> {
        while let Ok((m, generation)) = self.receiver.try_recv() {
            if generation.is_none() || generation == Some(self.generation) {
                return Some(m);
            }
        }
        None
    }
    fn sender(&self) -> MessageSender {
        MessageSender {
            inner: MessageSenderInner {
                queue: self.sender.clone(),
                notify: self.notify.clone(),
            },
        }
    }
    fn sender_with_gen(&self) -> MessageGenSender {
        MessageGenSender {
            inner: MessageSenderInner {
                queue: self.sender.clone(),
                notify: self.notify.clone(),
            },
            generation: self.generation,
        }
    }
    /// Increment the generation, orphaning senders from older generations
    fn increment_gen(&mut self) {
        self.generation += 1;
    }
}

impl MessageSender {
    fn send(&self, m: Message) {
        self.inner.send(m, None)
    }
}

impl MessageGenSender {
    fn send(&self, m: Message) {
        self.inner.send(m, Some(self.generation))
    }
}

impl MessageSenderInner {
    fn send(&self, m: Message, g: Option<u64>) {
        if self.queue.send((m, g)).is_ok() {
            if self.notify.send(()).is_err() {
                error!("notify returned an error");
            }
        } else {
            error!("sending returned an error");
        }
    }
}

struct Example {
    file_name: String,
    data: AppState,
}

pub struct App {
    data: World,
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    library: world::ShapeLibrary,
    examples: Vec<Example>,
    undo: state::Undo,

    /// File path used for loading and saving
    pub file: Option<std::path::PathBuf>,

    meta: state::Metadata,
    tree: egui_dock::DockState<gui::Tab>,
    syntax: egui_extras::syntax_highlighting::SyntectSettings,
    views: HashMap<BlockIndex, view::ViewData>,

    rx: MessageReceiver,
    script_state: ScriptState,

    platform: platform::Data,

    /// Show debug options and menu items in native build
    debug: bool,

    /// Shows the inspection UI (debug mode only)
    show_inspection_ui: bool,

    modal: Option<Modal>,
    quit_confirmed: bool,
    request_repaint: bool,

    /// Confirms that `meta.name` should be used for local saves
    local_name_confirmed: bool,
}

enum ScriptState {
    Done,
    Running { changed: bool },
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum NextAction {
    Quit,
    New,
    LoadFile { local: bool },
    LoadExample(AppState),
}

impl std::fmt::Debug for NextAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NextAction::Quit => f.debug_tuple("Quit").finish(),
            NextAction::New => f.debug_tuple("New").finish(),
            NextAction::LoadFile { local } => {
                f.debug_struct("Open").field("local", &local).finish()
            }
            NextAction::LoadExample(_) => {
                f.debug_tuple("LoadExample").field(&"..").finish()
            }
        }
    }
}

#[must_use]
#[allow(clippy::large_enum_variant)]
enum Modal {
    /// An action requires a check to discard unsaved changes
    Unsaved(NextAction),
    Error {
        title: String,
        message: String,
    },
    /// A download has been requested; populate `meta.name`
    Download {
        state: AppState,
        name: String,
    },
    SaveLocal {
        state: AppState,
        files: Vec<String>,
        name: String,
    },
    OpenLocal {
        files: Vec<String>,
        name: String,
    },
    ExportInProgress {
        target: platform::ExportTarget,
        cancel: fidget::render::CancelToken,
    },
    WaitForLoad,
    About,
}

impl std::fmt::Debug for Modal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Modal::Unsaved(u) => f.debug_tuple("Unsaved").field(u).finish(),
            Modal::Error { title, message } => f
                .debug_struct("Error")
                .field("title", title)
                .field("message", message)
                .finish(),
            Modal::Download { name, .. } => f
                .debug_struct("Download")
                .field("name", name)
                .field("state", &"..")
                .finish(),
            Modal::SaveLocal { name, .. } => f
                .debug_struct("SaveLocal")
                .field("name", name)
                .field("files", &"..")
                .field("state", &"..")
                .finish(),
            Modal::OpenLocal { name, .. } => f
                .debug_struct("OpenLocal")
                .field("name", name)
                .field("files", &"..")
                .finish(),
            Modal::WaitForLoad => f.debug_struct("WaitForLoad").finish(),
            Modal::ExportInProgress { target, .. } => f
                .debug_struct("ExportInProgress")
                .field("target", &format!("{target:?}"))
                .finish(),
            Modal::About => f.debug_struct("About").finish(),
        }
    }
}

impl App {
    /// Builds a new `App`
    ///
    /// Returns a tuple of the `App` and a channel which should trigger a
    /// repaint when it receives a message.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        debug: bool,
    ) -> (Self, flume::Receiver<()>) {
        // Install custom render pipelines
        let wgpu_state = cc.wgpu_render_state.as_ref().unwrap();
        painters::WgpuResources::install(wgpu_state);

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

        let mut examples = vec![];
        for (file_name, data) in examples::EXAMPLES {
            match AppState::deserialize(data) {
                Ok(data) => {
                    examples.push(Example {
                        file_name: file_name.to_string(),
                        data,
                    });
                }
                Err(e) => error!("example {file_name} is invalid: {e:?}"),
            }
        }

        let (ps, _) = bincode::serde::decode_from_slice(
            SYNTAX,
            bincode::config::standard(),
        )
        .unwrap();
        let ts = syntect::highlighting::ThemeSet::load_defaults();

        // XXX hack: `egui_extras::syntax_highlighting` will always use
        // `base16-mocha.dark`, so we'll use its name for `base16-eighties.dark`
        let mut syntax =
            egui_extras::syntax_highlighting::SyntectSettings { ps, ts };
        let s = syntax.ts.themes.remove("base16-eighties.dark").unwrap();
        syntax.ts.themes.insert("base16-mocha.dark".to_owned(), s);

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
            style.visuals = theme_visuals();
        });

        let (notify_tx, notify_rx) = flume::unbounded();
        let rx = MessageReceiver::new(notify_tx);
        let data = World::new();
        let undo = state::Undo::new(&data);
        let queue = rx.sender();
        let platform = platform::Data::new(queue);
        let app = Self {
            data,
            library: world::ShapeLibrary::build(),
            examples,
            tree: egui_dock::DockState::new(vec![]),
            script_state: ScriptState::Done,
            undo,
            file: None,
            syntax,
            views: HashMap::new(),
            meta: state::Metadata::default(),
            generation: std::sync::Arc::new(0.into()),
            platform,
            rx,
            debug,
            show_inspection_ui: false,
            modal: None,
            quit_confirmed: false,
            request_repaint: false,
            local_name_confirmed: false,
        };
        (app, notify_rx)
    }

    /// Gets our current `AppState`
    fn get_state(&self) -> AppState {
        AppState::new(&self.data, &self.views, &self.tree, &self.meta)
    }

    /// Loads an example by name, returning `false` if not found
    #[must_use]
    pub fn load_example(&mut self, target: &str) -> bool {
        let Some(e) = self.examples.iter().find(|e| e.file_name == target)
        else {
            return false;
        };
        self.load_from_state(e.data.clone());
        true
    }

    pub fn load_from_state(&mut self, state: AppState) {
        self.data = state.world.into();
        self.tree = state.dock;
        self.meta = state.meta;
        self.views = state
            .views
            .into_iter()
            .map(|(k, v)| (k, view::ViewData::from(view::ViewCanvas::from(v))))
            .collect();
        self.generation
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.undo = state::Undo::new(&self.data);
        self.rx.increment_gen(); // orphan previous tasks
    }

    pub fn start_world_rebuild(&mut self) {
        if let ScriptState::Running { changed } = &mut self.script_state {
            *changed = true;
            return;
        }

        // Send the world to a worker thread for re-evaluation
        let world = WorldState::from(&self.data);
        let tx = self.rx.sender_with_gen();
        rayon::spawn(move || {
            let world = World::from(world);
            tx.send(Message::RebuildWorld { world })
        });
        self.script_state = ScriptState::Running { changed: false };
    }

    fn draw_menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("\u{ea7b} New").clicked() {
                    self.on_new();
                }
                ui.separator();
                if cfg!(target_arch = "wasm32") {
                    // Web menu
                    if ui.button("\u{f093} Upload").clicked() {
                        self.on_load(false);
                    }
                    if ui.button("\u{f019} Download").clicked() {
                        self.on_download();
                    }
                    ui.separator();
                    if ui.button("\u{eb4b} Save").clicked() {
                        self.on_save_local();
                    }
                    if ui.button("\u{eb4a} Save As").clicked() {
                        self.on_save_as_local();
                    }
                    if ui.button("\u{f07c} Open").clicked() {
                        self.on_open();
                    }
                } else {
                    // Native menu items!
                    if ui.button("\u{eb4b} Save").clicked() {
                        self.on_save();
                    }
                    if ui.button("\u{eb4a} Save as").clicked() {
                        self.on_save_as();
                    }
                    if ui.button("\u{f07c} Open").clicked() {
                        self.on_open();
                    }
                    ui.separator();

                    // Special debug menu items to test web behavior
                    if self.debug {
                        ui.label("Debug zone:");
                        if ui.button("\u{f019} Download").clicked() {
                            self.on_download();
                        }
                        if ui.button("Save (local)").clicked() {
                            self.on_save_local();
                        }
                        if ui.button("Save As (local)").clicked() {
                            self.on_save_as_local();
                        }
                        if ui.button("Open (local)").clicked() {
                            self.on_load(true);
                        }
                        ui.separator();
                    }

                    if ui.button("\u{f0a48} Quit").clicked() {
                        self.on_quit(ctx);
                    }
                }
            });
            ui.menu_button("Edit", |ui| {
                ui.add_enabled_ui(self.undo.has_undo(&self.data), |ui| {
                    if ui.button("Undo").clicked() {
                        self.on_undo();
                    }
                });
                ui.add_enabled_ui(self.undo.has_redo(&self.data), |ui| {
                    if ui.button("Redo").clicked() {
                        self.on_redo();
                    }
                });
            });
            ui.menu_button("Examples", |ui| {
                let mut load_state = None;
                for e in &self.examples {
                    let name = e
                        .data
                        .meta
                        .description
                        .as_ref()
                        .unwrap_or(&e.file_name);
                    if ui.button(name).clicked() {
                        load_state = Some(e.data.clone())
                    }
                }
                if let Some(state) = load_state {
                    if self.undo.is_saved() {
                        self.load_from_state(state);
                    } else {
                        self.modal = Some(Modal::Unsaved(
                            NextAction::LoadExample(state),
                        ));
                    }
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("\u{eb32} About").clicked() {
                    self.on_about();
                }
                if self.debug {
                    ui.checkbox(&mut self.show_inspection_ui, "Debug");
                }
            });
            if cfg!(target_arch = "wasm32") || self.debug {
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let t = match &self.meta.name {
                            Some(name) => {
                                if self.undo.is_saved() {
                                    name.to_owned()
                                } else {
                                    format!("{name} [unsaved]")
                                }
                            }
                            None => "[no file name]".to_owned(),
                        };
                        ui.add_enabled(false, egui::Label::new(t))
                    },
                );
            }
        });
    }

    /// Draws a list of blocks into a caller-provided left panel
    ///
    /// Returns `true` if anything in the world has changed
    #[must_use]
    fn draw_block_list(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            // Draw the "new block" button at the bottom
            ui.add_space(5.0);
            egui::ComboBox::from_id_salt("new_script_block")
                .selected_text(gui::NEW_BLOCK)
                .width(0.0)
                .show_ui(ui, |ui| {
                    let mut index = usize::MAX;
                    let mut prev_category = None;
                    for (i, s) in self.library.shapes.iter().enumerate() {
                        if prev_category.is_some_and(|c| c != s.category) {
                            ui.separator();
                        }
                        ui.selectable_value(&mut index, i, &s.name);
                        prev_category = Some(s.category);
                    }
                    if index != usize::MAX {
                        let b = &self.library.shapes[index];
                        if self.data.new_block_from(b) {
                            changed = true;
                        }
                    }
                });
            ui.separator();
            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.block_list(ui) {
                        changed = true;
                    }
                });
            });
        });
        changed
    }

    /// Draws a blocking modal (if present in `self.modal`)
    fn draw_modal(&mut self, ctx: &egui::Context, window_size: egui::Vec2) {
        let Some(modal) = &mut self.modal else {
            return;
        };

        // Cancel certain modals if escape is pressed
        let (enter_pressed, escape_pressed) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if (matches!(
            modal,
            Modal::Unsaved(..)
                | Modal::Error { .. }
                | Modal::Download { .. }
                | Modal::SaveLocal { .. }
                | Modal::OpenLocal { .. }
                | Modal::About
        ) && escape_pressed)
            || (matches!(modal, Modal::Error { .. } | Modal::About)
                && enter_pressed)
        {
            self.modal = None;
            return;
        }

        // XXX weird hacky behavior
        let dialog_size =
            (window_size / 2.0).max(egui::Vec2::new(200.0, 200.0));

        // Block all interaction behind the modal
        let screen_rect = ctx.content_rect();
        let layer_id = egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("modal_layer"),
        );
        ctx.layer_painter(layer_id).rect_filled(
            screen_rect,
            0.0,
            egui::Color32::from_black_alpha(192),
        );
        egui::Area::new("modal_blocker_input".into())
            .order(egui::Order::Middle)
            .interactable(true)
            .fixed_pos(screen_rect.min)
            .show(ctx, |ui| {
                ui.allocate_rect(screen_rect, egui::Sense::all());
            });

        enum FileNameResponse {
            Ok(String),
            Cancel,
            None,
        }

        /// Helper function to get a file name
        fn dialog_name(
            ui: &mut egui::Ui,
            name: &mut String,
        ) -> Result<String, &'static str> {
            ui.add(
                egui::TextEdit::singleline(name).desired_width(f32::INFINITY),
            );
            let just_normal_characters = name.chars().all(|c| {
                c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'
            });
            let valid_extension = if let Some((_, ext)) = name.rsplit_once('.')
            {
                Some(ext == "half")
            } else {
                None
            };
            let err = if !just_normal_characters {
                Some("Invalid characters in name")
            } else if valid_extension == Some(false) {
                Some("Extension must be .half")
            } else if name.is_empty() {
                Some("Name cannot be empty")
            } else {
                None
            };
            if let Some(err) = err {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        ui.style().visuals.error_fg_color,
                        gui::WARN,
                    );
                    ui.label(err)
                });
                Err(err)
            } else {
                let mut name = name.clone();
                if valid_extension.is_none() {
                    name += ".half";
                }
                Ok(name)
            }
        }

        /// Helper function to show a pair of buttons
        fn dialog_buttons(
            ui: &mut egui::Ui,
            r: Result<String, &'static str>,
            label: &'static str,
            enter_pressed: bool,
        ) -> FileNameResponse {
            let w = ui.available_width();
            let initial_padding =
                (w - (w * 0.8 + ui.ctx().style().spacing.item_spacing.x)) / 2.0;
            let button_size = egui::Vec2::new(w * 0.4, 20.0);
            ui.add_space(5.0);
            ui.horizontal_top(|ui| {
                ui.add_space(initial_padding);
                let mut out = FileNameResponse::None;
                if ui
                    .add_enabled_ui(r.is_ok(), |ui| {
                        ui.add_sized(button_size, egui::Button::new(label))
                            .clicked()
                    })
                    .inner
                    || (r.is_ok() && enter_pressed)
                {
                    out = FileNameResponse::Ok(r.unwrap())
                }
                if ui
                    .add_sized(button_size, egui::Button::new("Cancel"))
                    .clicked()
                {
                    out = FileNameResponse::Cancel;
                }
                out
            })
            .inner
        }

        fn draw_modal_window<R>(
            ctx: &egui::Context,
            title: &str,
            dialog_size: egui::Vec2,
            add_contents: impl FnOnce(&mut egui::Ui) -> R,
        ) -> R {
            // XXX width is weirdly stateful if you shrink and expand
            egui::Window::new(title)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .min_width(dialog_size.x)
                .max_width(dialog_size.x)
                .default_width(dialog_size.x)
                .min_height(dialog_size.y)
                .max_height(dialog_size.y)
                .default_height(dialog_size.x)
                .frame(egui::Frame::popup(&ctx.style()))
                .show(ctx, add_contents)
                .unwrap()
                .inner
                .unwrap()
        }

        fn file_selector(
            ui: &mut egui::Ui,
            files: &[String],
            name: &mut String,
        ) {
            use egui::containers::scroll_area::ScrollBarVisibility;
            let height = egui::TextStyle::Body.resolve(ui.style()).size;
            egui::ScrollArea::vertical()
                .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
                .max_width(f32::INFINITY)
                .auto_shrink(false)
                .show_rows(ui, height, files.len(), |ui, row_range| {
                    // Hide button backgrounds
                    ui.style_mut().visuals.widgets.inactive.weak_bg_fill =
                        egui::Color32::TRANSPARENT;
                    for i in row_range {
                        let r = ui.add(egui::Button::new(&files[i]));
                        if r.clicked() {
                            *name = files[i].clone();
                        }
                    }
                });
        }

        match modal {
            Modal::Unsaved(m) => {
                let s = match m {
                    NextAction::New => "New",
                    NextAction::LoadFile { .. } => "Load file",
                    NextAction::Quit => "Quit",
                    NextAction::LoadExample(..) => "Load example",
                };
                let r = draw_modal_window(ctx, s, dialog_size, |ui| {
                    ui.label("You have unsaved changes. Continue?");
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        if ui.button("Ok").clicked() {
                            true
                        } else if ui.button("Cancel").clicked() {
                            self.modal = None;
                            false
                        } else {
                            false
                        }
                    })
                    .inner
                });
                if r {
                    let Some(Modal::Unsaved(d)) = self.modal.take() else {
                        unreachable!()
                    };
                    match d {
                        NextAction::New => {
                            self.new_file();
                        }
                        NextAction::Quit => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            self.quit_confirmed = true;
                        }
                        NextAction::LoadFile { local } => {
                            if local {
                                self.open_local()
                            } else {
                                self.platform_open();
                            }
                        }
                        NextAction::LoadExample(s) => self.load_from_state(s),
                    }
                }
            }
            Modal::Error { title, message } => {
                let r = draw_modal_window(ctx, title, dialog_size, |ui| {
                    ui.add(
                        egui::Label::new(message.as_str())
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                    ui.add_space(5.0);
                    ui.horizontal(|ui| ui.button("oopsie whoopsie").clicked())
                        .inner
                });
                if r {
                    self.modal = None;
                }
            }
            Modal::Download { state, name } => {
                let r = draw_modal_window(
                    ctx,
                    "Set download name",
                    dialog_size,
                    |ui| {
                        ui.add_space(5.0);
                        let r = dialog_name(ui, name);
                        dialog_buttons(ui, r, "Download", enter_pressed)
                    },
                );
                match r {
                    FileNameResponse::Ok(name) => {
                        let state = std::mem::take(state);
                        self.modal = None;
                        self.download_file(&name, state);
                        self.meta.name = Some(name);
                    }
                    FileNameResponse::Cancel => self.modal = None,
                    FileNameResponse::None => (),
                }
            }
            Modal::SaveLocal { state, files, name } => {
                let r = draw_modal_window(
                    ctx,
                    "Save to local storage",
                    dialog_size,
                    |ui| {
                        file_selector(ui, files, name);
                        let r = dialog_name(ui, name);
                        if r.as_ref().is_ok_and(|n| files.contains(n)) {
                            ui.add_space(5.0);
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    ui.style().visuals.warn_fg_color,
                                    gui::WARN,
                                );
                                ui.label("Overwriting existing file")
                            });
                        }
                        ui.add_space(5.0);
                        dialog_buttons(ui, r, "Save", enter_pressed)
                    },
                );
                match r {
                    FileNameResponse::Ok(name) => {
                        let mut state = std::mem::take(state);
                        self.modal = None;
                        state.meta.name = Some(name.clone());
                        self.platform
                            .save_to_local_storage(&name, &state.serialize());
                        self.undo.mark_saved(state.world);
                        self.meta.name = Some(name);
                        self.local_name_confirmed = true;
                    }
                    FileNameResponse::Cancel => self.modal = None,
                    FileNameResponse::None => (),
                }
            }
            Modal::OpenLocal { files, name } => {
                let r = draw_modal_window(
                    ctx,
                    "Open from local storage",
                    dialog_size,
                    |ui| {
                        file_selector(ui, files, name);
                        let mut r = dialog_name(ui, name);
                        if r.as_ref().is_ok_and(|n| !files.contains(n)) {
                            r = Err("no file selected");
                        }
                        ui.add_space(5.0);
                        dialog_buttons(ui, r, "Open", enter_pressed)
                    },
                );
                match r {
                    FileNameResponse::Ok(name) => {
                        let data = self.platform.read_from_local_storage(&name);
                        self.modal = match AppState::deserialize(&data) {
                            Ok(state) => {
                                self.load_from_state(state);
                                self.local_name_confirmed = true;
                                None
                            }
                            Err(e) => Some(Modal::Error {
                                title: "Failed to load".to_owned(),
                                message: format!(
                                    "{:#}",
                                    anyhow::Error::from(e)
                                ),
                            }),
                        };
                    }
                    FileNameResponse::Cancel => self.modal = None,
                    FileNameResponse::None => (),
                }
            }
            Modal::WaitForLoad => {
                // Nothing to do here, just block the screen
            }
            Modal::ExportInProgress { cancel, .. } => {
                let r = draw_modal_window(
                    ctx,
                    "Export in progress",
                    dialog_size,
                    |ui| ui.button("Cancel").clicked(),
                );
                if r {
                    cancel.cancel();
                }
            }
            Modal::About => {
                egui::Window::new("About")
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .collapsible(false)
                    .resizable(false)
                    .default_width(0.0)
                    .order(egui::Order::Foreground)
                    .frame(egui::Frame::popup(&ctx.style()))
                    .show(ctx, |ui| {
                        ui.add_space(5.0);
                        use git_version::git_version;
                        const VERSION: &str = git_version!(
                            prefix = "git:",
                            cargo_prefix = "cargo:",
                            fallback = "unknown"
                        );
                        ui.horizontal(|ui| {
                            ui.label("Version: ");
                            ui.hyperlink_to(
                                egui::RichText::new(VERSION).underline(),
                                format!(
                                    "https://github.com/mkeeter/halfspace\
                                /commit/{}",
                                    VERSION
                                        .trim_end_matches("-modified")
                                        .trim_start_matches("git:")
                                        .trim_start_matches("cargo:")
                                ),
                            );
                        });
                        ui.add_space(5.0);
                        ui.vertical_centered(|ui| {
                            if ui.button("Okay").clicked() {
                                self.modal = None;
                            }
                        });
                    });
            }
        };
    }

    #[must_use]
    fn draw_tab_region(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
    ) -> bool {
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
            tx: &self.rx.sender_with_gen(),
            out: &mut io_out,
        };
        egui_dock::DockArea::new(&mut self.tree)
            .style(egui_dock::Style::from_egui(ctx.style().as_ref()))
            .show_leaf_collapse_buttons(false)
            .show_leaf_close_all_buttons(false)
            .show_inside(ui, &mut bw);
        let mut changed = false;
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
                        changed = true;
                    }
                    ViewResponse::REDRAW => {
                        self.request_repaint = true;
                    }
                    _ => panic!("invalid flag"),
                }
            }
        }
        changed
    }

    #[must_use]
    fn draw_ui(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        egui::Panel::top("menu").show(ctx, |ui| {
            ui.add_space(2.0);
            self.draw_menu(ctx, ui);
            ui.add_space(2.0);
        });

        changed |= egui::Panel::left("left_panel")
            .min_size(250.0)
            .show(ctx, |ui| self.draw_block_list(ui))
            .inner;

        let size = egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .inner_margin(0.0)
                    .fill(egui::Color32::TRANSPARENT),
            )
            .show(ctx, |ui| {
                let size = ui.available_size();
                changed |= self.draw_tab_region(ctx, ui);
                size
            })
            .inner;

        // Draw optional modals
        self.draw_modal(ctx, size);

        if self.show_inspection_ui {
            egui::Window::new("Debug").show(ctx, |ui| {
                ctx.style_ui(ui, egui::Theme::Light);
            });
        }

        changed
    }

    fn on_download(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring download while modal is active");
        } else {
            let state = self.get_state();
            if let Some(f) = self.meta.name.take() {
                self.download_file(&f, state);
                self.meta.name = Some(f);
            } else {
                self.modal = Some(Modal::Download {
                    state,
                    name: String::new(),
                });
            }
        }
    }

    fn download_file(&mut self, f: &str, state: AppState) {
        let json_str = state.serialize();
        match self.platform.download_file(f, json_str.as_bytes()) {
            None => self.undo.mark_saved(state.world),
            Some(d) => self.modal = Some(d),
        }
    }

    fn on_quit(&mut self, ctx: &egui::Context) {
        if self.undo.is_saved() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            self.modal = Some(Modal::Unsaved(NextAction::Quit));
        }
    }

    fn on_redo(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring redo while modal is active");
        } else if let Some(prev) = self.undo.redo(&self.data) {
            debug!("got redo state");
            let prev = prev.clone();
            self.restore_world_state(prev);
        } else {
            // XXX show a dialog or something?
            warn!("no redo available");
        }
    }

    fn on_about(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring about while modal is active");
        } else {
            self.modal = Some(Modal::About);
        }
    }

    fn on_open(&mut self) {
        // "Open" is a local load for WASM32 builds, and a native load otherwise
        let is_local = cfg!(target_arch = "wasm32");
        self.on_load(is_local);
    }

    fn on_load(&mut self, local: bool) {
        if self.modal.is_some() {
            warn!("ignoring load while modal is active");
        } else if self.undo.is_saved() {
            if local {
                self.open_local()
            } else {
                self.platform_open()
            }
        } else {
            self.modal = Some(Modal::Unsaved(NextAction::LoadFile { local }));
        }
    }

    fn open_local(&mut self) {
        let files = self.platform.list_local_storage();
        self.modal = Some(Modal::OpenLocal {
            files,
            name: String::new(),
        });
    }

    fn on_save_local(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring save while modal is active");
        } else if let Some(name) = &self.meta.name {
            let state = self.get_state();
            // If we have previously confirmed the local name through a save or
            // open dialog, then we'll overwrite an existing file.  On the other
            // hand, if we've only set the local name through downloads, we will
            // confirm that it's okay to overwrite.
            if self.local_name_confirmed {
                self.platform
                    .save_to_local_storage(name, &state.serialize());
                self.undo.mark_saved(state.world);
            } else {
                let files = self.platform.list_local_storage();
                if files.contains(name) {
                    self.modal = Some(Modal::SaveLocal {
                        state,
                        files,
                        name: name.clone(),
                    });
                } else {
                    self.platform
                        .save_to_local_storage(name, &state.serialize());
                    self.undo.mark_saved(state.world);
                }
            }
        } else {
            self.modal = Some(Modal::SaveLocal {
                files: self.platform.list_local_storage(),
                state: self.get_state(),
                name: String::new(),
            });
        }
    }

    fn on_save_as_local(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring save while modal is active");
        } else {
            self.modal = Some(Modal::SaveLocal {
                files: self.platform.list_local_storage(),
                state: self.get_state(),
                name: self.meta.name.clone().unwrap_or_default(),
            });
        }
    }

    fn on_save(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring save while modal is active");
        } else {
            self.platform_save();
        }
    }

    fn on_save_as(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring save as while modal is active");
        } else {
            self.platform_save_as();
        }
    }

    fn on_undo(&mut self) {
        if self.modal.is_some() {
            warn!("ignoring undo while modal is active");
        } else if let Some(prev) = self.undo.undo(&self.data) {
            debug!("got undo state");
            let prev = prev.clone();
            self.restore_world_state(prev);
        } else {
            // XXX show a dialog or something?
            warn!("no undo available");
        }
    }

    fn on_new(&mut self) {
        if self.modal.is_some() {
            warn!("cannot execute on_new with active modal");
        } else if self.undo.is_saved() {
            self.new_file()
        } else {
            self.modal = Some(Modal::Unsaved(NextAction::New))
        }
    }

    /// Resets our file to an empty state, setting `self.request_repaint`
    fn new_file(&mut self) {
        self.file = None;
        self.load_from_state(AppState::default());
        self.request_repaint = true;
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        Self::reset_wgpu_state(frame);
        self.update_undo_redo(ctx);
        self.handle_messages();
        self.check_shortcuts(ctx);
        self.intercept_quit(ctx);

        if self.draw_ui(ctx) {
            self.start_world_rebuild();
        }

        self.platform_update_title(ctx);

        if std::mem::take(&mut self.request_repaint) {
            ctx.request_repaint();
        }
    }
}

impl App {
    /// Pick the characteristic matrix for each block
    ///
    /// The matrix is the view's matrix (if present); otherwise, it's the next
    /// available characteristic matrix.  If no views are open, then we pick a
    /// reasonable default value.
    fn characteristic_matrices(
        &self,
    ) -> HashMap<BlockIndex, nalgebra::Matrix4<f32>> {
        let mut mats = self
            .data
            .order
            .iter()
            .map(|index| {
                (
                    index,
                    self.views.get(index).map(|v| v.characteristic_matrix()),
                )
            })
            .collect::<Vec<_>>();
        let mut last_mat =
            nalgebra::Scale3::new(0.01, 0.01, 0.01).to_homogeneous();
        for (_i, s) in mats.iter_mut().rev() {
            if let Some(s) = s {
                last_mat = *s;
            } else {
                *s = Some(last_mat);
            }
        }
        mats.into_iter()
            .map(|(i, s)| (*i, s.unwrap()))
            .collect::<HashMap<_, _>>()
    }

    /// Draws the list of blocks
    ///
    /// Returns `true` if anything changed
    #[must_use]
    fn block_list(&mut self, ui: &mut egui::Ui) -> bool {
        // Draw blocks
        let mut to_delete = HashSet::new();
        let mut changed = false;
        let mut to_export = None;
        let last = self.data.order.last().cloned();

        let block_mats = self.characteristic_matrices();

        // XXX there is a drag-and-drop implementation that's built into egui,
        // see `egui_demo_lib/src/demo/drag_and_drop.rs`
        let r = dnd(ui, "dnd").show_vec(
            &mut self.data.order,
            |ui, index, handle, state| {
                let block = self.data.blocks.get_mut(index).unwrap();
                let mut tree =
                    gui::DockStateEditor::new(*index, &mut self.tree);

                // If we have an open view but block is (1) valid and (2) no
                // longer defines a view, then close the view.  We'll leave the
                // view open if the block isn't valid, to prevent views from
                // flicking in and out as a script is edited.
                let block_defines_view = block.has_view();
                if tree.has_view() && block.is_valid() && !block_defines_view {
                    tree.close_view();
                }

                let flags = gui::BlockUiFlags {
                    is_last: Some(*index) == last,
                    is_open: tree.has_script(),
                    is_dragged: state.dragged,
                    is_view_open: if tree.has_view() {
                        Some(true)
                    } else if block_defines_view {
                        Some(false)
                    } else {
                        None
                    },
                };
                let mat = block_mats[index];
                let r =
                    gui::draggable_block(ui, *index, block, flags, mat, handle);
                if r.contains(BlockResponse::DELETE) {
                    to_delete.insert(*index);
                    tree.close_view();
                    tree.close_script();
                }
                if r.contains(BlockResponse::TOGGLE_EDIT) {
                    tree.toggle_script();
                }
                if r.contains(BlockResponse::TOGGLE_VIEW) {
                    tree.toggle_view();
                }
                if r.contains(BlockResponse::FOCUS_ERR) {
                    tree.toggle_script();
                }
                if r.contains(BlockResponse::EXPORT) {
                    let world::Block::Script(s) = block else {
                        panic!("can't export from non-script block");
                    };
                    let Some(data) = &s.data else {
                        panic!("can't export without data");
                    };
                    let Some(e) = &data.export else {
                        panic!("can't export without export request");
                    };
                    if to_export.is_some() {
                        error!("multiple export requests found");
                    } else {
                        to_export = Some(e.clone());
                    }
                }
                changed |= r.contains(BlockResponse::CHANGED);
            },
        );
        if r.final_update().is_some() {
            changed = true;
        }

        // Post-processing: edit blocks based on button presses
        changed |= self.data.retain(|index| !to_delete.contains(index));
        self.views.retain(|index, _| !to_delete.contains(index));

        match to_export {
            Some(world::ExportRequest::Mesh {
                tree,
                min,
                max,
                feature_size,
            }) => {
                if self.modal.is_none()
                    && let Some(target) = self.platform_select_download("stl")
                {
                    let cancel = fidget::render::CancelToken::new();
                    let cancel_ = cancel.clone();
                    let tx = self.rx.sender();
                    rayon::spawn(move || {
                        let r = export::build_stl(
                            tree,
                            min,
                            max,
                            feature_size,
                            cancel_,
                        );
                        tx.send(Message::ExportComplete(r))
                    });
                    self.modal =
                        Some(Modal::ExportInProgress { target, cancel });
                }
            }
            Some(world::ExportRequest::Image {
                scene,
                min,
                max,
                resolution,
            }) => {
                if self.modal.is_none()
                    && let Some(target) = self.platform_select_download("png")
                {
                    let cancel = fidget::render::CancelToken::new();
                    let cancel_ = cancel.clone();
                    let tx = self.rx.sender();
                    rayon::spawn(move || {
                        let r = export::build_image(
                            scene, min, max, resolution, cancel_,
                        );
                        tx.send(Message::ExportComplete(r))
                    });
                    self.modal =
                        Some(Modal::ExportInProgress { target, cancel });
                }
            }
            None => (),
        }

        changed
    }

    pub fn restore_world_state(&mut self, state: WorldState) {
        self.data = state.into();
        self.tree
            .retain_tabs(|t| self.data.blocks.contains_key(&t.index));
        self.views.retain(|i, _| self.data.blocks.contains_key(i));
        self.start_world_rebuild();
        self.request_repaint = true;
    }

    /// Resets [`WgpuResources`](painters::WgpuResources) attached to a frame
    fn reset_wgpu_state(frame: &eframe::Frame) {
        let wgpu_state = frame.wgpu_render_state().unwrap();
        if let Some(r) = wgpu_state
            .renderer
            .write()
            .callback_resources
            .get_mut::<painters::WgpuResources>()
        {
            r.reset();
        }
    }

    /// Updates the undo tracking system
    fn update_undo_redo(&mut self, ctx: &egui::Context) {
        let (is_dragging, drag_released) =
            ctx.input(|i| (i.pointer.any_down(), i.pointer.any_released()));
        if drag_released {
            self.undo.checkpoint(&self.data);
        } else if !is_dragging {
            self.undo.feed_state(&self.data);
        }
    }

    /// Receive updates from the worker pool
    ///
    /// These may include evaluation results, rendering, and dialog states
    fn handle_messages(&mut self) {
        while let Some(m) = self.rx.try_recv() {
            self.handle_message(m);
        }
    }

    /// Handles a single message
    fn handle_message(&mut self, m: Message) {
        match m {
            Message::RebuildWorld { world } => {
                self.data.import_data(world);
                let ScriptState::Running { changed } = std::mem::replace(
                    &mut self.script_state,
                    ScriptState::Done,
                ) else {
                    panic!("got RebuildWorld while script wasn't running");
                };
                if changed {
                    self.start_world_rebuild();
                }
            }
            Message::RenderView {
                block,
                generation,
                data,
                start_time,
            } => {
                if let Some(e) = self.views.get_mut(&block) {
                    e.update(generation, data, start_time.elapsed())
                }
            }
            Message::Loaded { state, path } => match self.modal {
                Some(Modal::WaitForLoad) => {
                    self.load_from_state(state);
                    self.file = path;
                    self.modal = None;
                    self.local_name_confirmed = true;
                }
                _ => warn!(
                    "received Loaded with unexpected modal {:?}",
                    self.modal
                ),
            },
            Message::CancelLoad => match self.modal {
                Some(Modal::WaitForLoad) => {
                    self.modal = None;
                }
                _ => warn!(
                    "received CancelLoad with unexpected modal {:?}",
                    self.modal
                ),
            },
            Message::LoadFailed { title, message } => match self.modal {
                Some(Modal::WaitForLoad) => {
                    self.modal = Some(Modal::Error { title, message });
                }
                _ => warn!(
                    "received Loadfailed with unexpected modal {:?}",
                    self.modal
                ),
            },
            Message::ExportComplete(r) => match &self.modal {
                Some(Modal::ExportInProgress { target, .. }) => match r {
                    Err(export::ExportError::Cancelled) => self.modal = None,
                    Err(e) => {
                        self.modal = Some(Modal::Error {
                            title: "Export failed".to_owned(),
                            message: format!("{:#}", anyhow::Error::from(e)),
                        });
                    }
                    Ok(data) => match target.save(&data) {
                        Ok(()) => self.modal = None,
                        Err(e) => {
                            self.modal = Some(Modal::Error {
                                title: "Export failed".to_owned(),
                                message: format!(
                                    "{:#}",
                                    anyhow::Error::from(e)
                                ),
                            })
                        }
                    },
                },
                _ => warn!(
                    "received ExportComplete with unexpected modal {:?}",
                    self.modal,
                ),
            },
        }
    }

    fn check_shortcuts(&mut self, ctx: &egui::Context) {
        let mut quit_requested = false;
        if self.modal.is_none() {
            ctx.input_mut(|i| {
                if i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::MAC_CMD,
                    egui::Key::N,
                )) {
                    self.on_new();
                }
                if i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::MAC_CMD,
                    egui::Key::Q,
                )) {
                    // We can't call on_quit directly because ctx is locked
                    quit_requested = true;
                }
                if i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::MAC_CMD | egui::Modifiers::SHIFT,
                    egui::Key::S,
                )) {
                    self.on_save_as();
                }
                if i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::MAC_CMD,
                    egui::Key::S,
                )) {
                    self.on_save();
                }
                if i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::MAC_CMD,
                    egui::Key::O,
                )) {
                    self.on_open();
                }
                if i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::MAC_CMD | egui::Modifiers::SHIFT,
                    egui::Key::Z,
                )) {
                    self.on_redo();
                }
                if i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::MAC_CMD,
                    egui::Key::Z,
                )) {
                    self.on_undo();
                }
            });
        }
        if quit_requested {
            self.on_quit(ctx);
        }
    }

    /// Intercept window-level quit commands and pop up a modal if unsaved
    fn intercept_quit(&mut self, ctx: &egui::Context) {
        // Note that this doesn't actually work on macOS right now, see
        // https://github.com/emilk/egui/issues/7115
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.undo.is_saved() || self.quit_confirmed {
                // do nothing - we will close
            } else {
                // Open a quit modal and cancel the close request
                self.modal = Some(Modal::Unsaved(NextAction::Quit));
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }
    }
}

bitflags::bitflags! {
    /// Represents a set of flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, )]
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
        /// Export this block
        const EXPORT        = (1 << 5);
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

////////////////////////////////////////////////////////////////////////////////

/// Inconsolata with additional icons, included in the binary
const INCONSOLATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fonts/InconsolataNerdFontPropo-Regular.ttf"
));

/// `SyntextSet` for Rhai, generated by `build.rs` and serialized with bincode
const SYNTAX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/syntax.bin"));

mod examples {
    include!(concat!(env!("OUT_DIR"), "/examples.rs"));
}
