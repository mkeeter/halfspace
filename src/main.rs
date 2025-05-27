use egui_dnd::dnd;
use std::collections::HashSet;

mod gui;
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

struct App {
    data: World,
    tree: egui_dock::DockState<BlockIndex>,
    syntax: egui_extras::syntax_highlighting::SyntectSettings,

    /// Pool to execute off-thread evaluation
    pool: rayon::ThreadPool,
    rx: std::sync::mpsc::Receiver<World>,
    tx: std::sync::mpsc::Sender<World>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
            syntax,
            pool: rayon::ThreadPoolBuilder::default().build().unwrap(),
            tx,
            rx,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Receive new data from the worker thread
        while let Ok(world) = self.rx.try_recv() {
            self.data = world;
        }
        let mut changed = false;
        egui::SidePanel::left("left_panel")
            .min_width(250.0)
            .show(ctx, |ui| {
                changed |= self.left(ui);
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

                let mut bw = gui::BoundWorld {
                    world: &mut self.data,
                    syntax: &self.syntax,
                    changed: &mut changed,
                };
                egui_dock::DockArea::new(&mut self.tree)
                    .style(egui_dock::Style::from_egui(ctx.style().as_ref()))
                    .show_leaf_collapse_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .show_inside(ui, &mut bw);
            });
        if changed {
            // Send the world to a worker thread for re-evaluation
            let mut world = self.data.without_state();
            let ctx = ctx.clone();
            let tx = self.tx.clone();
            self.pool.install(move || {
                world.rebuild();
                if tx.send(world).is_ok() {
                    ctx.request_repaint();
                }
            });
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
                let tab_location = self.tree.find_tab(index);
                let r = gui::draggable_block(
                    ui,
                    *index,
                    self.data.blocks.get_mut(index).unwrap(),
                    tab_location.is_some(),
                    Some(*index) == last,
                    handle,
                    state,
                );
                if r.contains(BlockResponse::DELETE) {
                    to_delete.insert(*index);
                    if let Some(tab_location) = tab_location {
                        self.tree.remove_tab(tab_location).unwrap();
                    }
                }
                if r.contains(BlockResponse::TOGGLE_EDIT) {
                    if let Some(tab_location) = tab_location {
                        self.tree.remove_tab(tab_location).unwrap();
                    } else {
                        self.tree.push_to_focused_leaf(*index);
                    }
                }
                if r.contains(BlockResponse::FOCUS_ERR) {
                    if let Some(tab_location) = tab_location {
                        self.tree.set_active_tab(tab_location)
                    } else {
                        self.tree.push_to_focused_leaf(*index);
                    }
                }
                changed |= r.contains(BlockResponse::CHANGED);
            },
        );

        // Post-processing: edit blocks based on button presses
        changed |= self.data.retain(|index| !to_delete.contains(index));

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
        const DELETE = 0b00000001;
        /// Request to toggle the edit window
        const TOGGLE_EDIT = 0b00000010;
        /// Request to focus the edit window
        const FOCUS_ERR = 0b00000100;
        /// The block has changed
        const CHANGED = 0b00001000;
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
