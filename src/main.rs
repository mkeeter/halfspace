use egui_dnd::dnd;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;

mod draw;
mod gui;
mod view;
mod world;

use world::{BlockIndex, World};

pub fn main() -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions::default();

    eframe::run_native(
        "halfspace",
        native_options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
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
        f.write_all(b"HALF")?;
        const FILE_VERSION: u32 = 1;
        f.write_all(&FILE_VERSION.to_le_bytes())?;
        f.write_all(&state_data)?;
        f.flush()?;
        Ok(())
    }
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
                        let current_gen = gen_handle
                            .load(std::sync::atomic::Ordering::Acquire);
                        if current_gen == generation {
                            world.rebuild();
                            // Re-check generation before sending
                            let current_gen = gen_handle
                                .load(std::sync::atomic::Ordering::Acquire);
                            if current_gen == generation
                                && tx
                                    .send(Message::RebuildWorld {
                                        generation,
                                        world,
                                    })
                                    .is_ok()
                            {
                                ctx.request_repaint();
                            }
                        }
                    })
                }
                AppResponse::QUIT => {
                    std::process::exit(0);
                }
                AppResponse::SAVE => {
                    if self.file.is_none() {
                        for i in 0..100 {
                            if let Ok(f) = std::fs::File::create_new(format!(
                                "model_{i}.half"
                            )) {
                                self.file = Some(f);
                                break;
                            }
                        }
                        if self.file.is_none() {
                            panic!("could not create file");
                        }
                    }
                    self.write_to_file();
                }
                _ => panic!("invalid flag"),
            }
        }
    }
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
