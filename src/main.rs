use clap::Parser;
use eframe::egui_wgpu::wgpu;
use egui_dnd::dnd;
use log::info;
use log::warn;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, Write};

mod gui;
mod painters;
mod render;
mod shapes;
mod state;
mod view;
mod world;

use state::AppState;
use world::{BlockIndex, World};

/// An experimental CAD tool
#[derive(clap::Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// File to edit (created if not present)
    target: Option<std::path::PathBuf>,
}

/// Manually open a WebGPU session with `float32-filterable`
pub fn wgpu_setup() -> egui_wgpu::WgpuSetupExisting {
    let instance = wgpu::Instance::default();

    let adapter = pollster::block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        },
    ))
    .expect("Failed to find an appropriate adapter");

    let adapter_features = adapter.features();
    assert!(
        adapter_features.contains(wgpu::Features::FLOAT32_FILTERABLE),
        "Adapter does not support float32-filterable"
    );

    let required_features = wgpu::Features::FLOAT32_FILTERABLE;

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("Device with float32-filterable"),
            required_features,
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .expect("Failed to create device");

    egui_wgpu::WgpuSetupExisting {
        instance,
        adapter,
        device,
        queue,
    }
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

pub fn main() -> Result<(), eframe::Error> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();
    let args = Args::parse();

    let mut native_options = eframe::NativeOptions::default();
    native_options.wgpu_options.wgpu_setup = wgpu_setup().into();
    eframe::run_native(
        "halfspace",
        native_options,
        Box::new(|cc| {
            let mut app = App::new(cc);
            if let Some(filename) = args.target {
                let mut f = std::fs::File::options()
                    .create(true)
                    .append(true)
                    .read(true)
                    .open(&filename)?;
                f.seek(std::io::SeekFrom::End(0))?;
                let file_length = f.stream_position()?;
                app.file = Some((filename, f));
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
        start_time: std::time::Instant,
        data: view::ViewImage,
    },
}

struct App {
    data: World,
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    library: shapes::ShapeLibrary,

    file: Option<(std::path::PathBuf, std::fs::File)>,

    tree: egui_dock::DockState<gui::Tab>,
    syntax: egui_extras::syntax_highlighting::SyntectSettings,
    views: HashMap<BlockIndex, view::ViewData>,

    rx: std::sync::mpsc::Receiver<Message>,
    tx: std::sync::mpsc::Sender<Message>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
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

        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            data: World::new(),
            library: shapes::ShapeLibrary::build(),
            tree: egui_dock::DockState::new(vec![]),
            file: None,
            syntax,
            views: HashMap::new(),
            generation: std::sync::Arc::new(0.into()),
            tx,
            rx,
        }
    }

    /// Writes to the file in `self.file`
    ///
    /// # Panics
    /// If `self.file` is `None`
    fn write_to_file(&mut self) -> std::io::Result<()> {
        let state = AppState::new(&self.data, &self.views, &self.tree);
        let json_str = serde_json::to_string_pretty(&state)?;
        let (p, f) = self.file.as_mut().unwrap();
        info!("saving to {:?}", p);
        f.rewind()?;
        f.set_len(0)?;
        f.write_all(json_str.as_bytes())?;
        f.flush()?;
        Ok(())
    }

    /// Loads from the file in `self.file`
    ///
    /// # Panics
    /// If `self.file` is `None`
    fn load_from_file(&mut self) -> Result<(), ReadError> {
        let (_, f) = self.file.as_mut().unwrap();

        f.seek(std::io::SeekFrom::Start(0))?;
        let mut data = vec![];
        f.read_to_end(&mut data)?;
        let s = std::str::from_utf8(&data)?;
        let state = AppState::deserialize(s)?;
        self.restore_from_state(state);
        Ok(())
    }

    fn restore_from_state(&mut self, state: AppState) {
        self.data = state.world.into();
        self.tree = state.dock;
        self.views = state
            .views
            .into_iter()
            .map(|(k, v)| (k, view::ViewData::from(view::ViewCanvas::from(v))))
            .collect();
        self.generation
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx = tx; // use a new channel to orphan previous tasks
        self.rx = rx;
    }

    fn start_world_rebuild(&self, ctx: &egui::Context) {
        // Send the world to a worker thread for re-evaluation
        let world = state::WorldState::from(&self.data);
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
                let world = World::from(world);
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

#[derive(thiserror::Error, Debug)]
enum ReadError {
    #[error("io error encountered when reading file")]
    IoError(#[from] std::io::Error),

    #[error("file is not UTF-8")]
    NotUtf8(#[from] std::str::Utf8Error),

    #[error("could not parse JSON")]
    ParseError(#[from] serde_json::Error),

    #[error("bad tag: expected {expected}, got {actual}")]
    BadTag { expected: String, actual: String },

    #[error(
        "file is too new: our version is {expected_major}.{expected_minor}, \
         file's is {actual_major}.{actual_minor}"
    )]
    TooNew {
        expected_major: usize,
        expected_minor: usize,
        actual_major: usize,
        actual_minor: usize,
    },
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let wgpu_state = frame.wgpu_render_state().unwrap();
        if let Some(r) = wgpu_state
            .renderer
            .write()
            .callback_resources
            .get_mut::<painters::WgpuResources>()
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
                    data,
                    start_time,
                } => {
                    if let Some(e) = self.views.get_mut(&block) {
                        e.update(generation, data, start_time.elapsed())
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

                ui.with_layout(
                    egui::Layout::bottom_up(egui::Align::LEFT),
                    |ui| {
                        // Draw the "new block" button at the bottom
                        ui.add_space(5.0);
                        egui::ComboBox::from_id_salt("new_script_block")
                            .selected_text(gui::NEW_BLOCK)
                            .width(0.0)
                            .show_ui(ui, |ui| {
                                let mut index = usize::MAX;
                                let mut prev_category = None;
                                for (i, s) in
                                    self.library.shapes.iter().enumerate()
                                {
                                    if prev_category
                                        .is_some_and(|c| c != s.category)
                                    {
                                        ui.separator();
                                    }
                                    ui.selectable_value(&mut index, i, &s.name);
                                    prev_category = Some(s.category);
                                }
                                if index != usize::MAX {
                                    let b = &self.library.shapes[index];
                                    if self.data.new_block_from(b) {
                                        out |= AppResponse::WORLD_CHANGED;
                                    }
                                }
                            });
                        ui.separator();
                        ui.with_layout(
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                egui::ScrollArea::vertical().show(ui, |ui| {
                                    if self.block_list(ui) {
                                        out |= AppResponse::WORLD_CHANGED;
                                    }
                                });
                            },
                        );
                    },
                );
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

        // Handle app-level actions
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
                            let filename = std::path::PathBuf::from(format!(
                                "model_{i}.half"
                            ));
                            if let Ok(f) = std::fs::File::options()
                                .create_new(true)
                                .read(true)
                                .write(true)
                                .open(&filename)
                            {
                                self.file = Some((filename, f));
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
                let block_defines_view =
                    block.data.as_ref().is_some_and(|s| s.view.is_some());
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
                changed |= r.contains(BlockResponse::CHANGED);
            },
        );
        if r.final_update().is_some() {
            changed = true;
        }

        // Post-processing: edit blocks based on button presses
        changed |= self.data.retain(|index| !to_delete.contains(index));
        self.views.retain(|index, _| !to_delete.contains(index));

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
