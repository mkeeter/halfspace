use egui_dnd::dnd;
use std::collections::{HashMap, HashSet};

use fidget::{
    context::Tree,
    shapes::{Vec2, Vec3},
};

#[allow(unused)]
enum Value {
    Float(f64),
    Vec2(Vec2),
    Vec3(Vec3),
    Tree(Tree),
    String(String),
    Dynamic(rhai::Dynamic),
}

impl From<rhai::Dynamic> for Value {
    fn from(d: rhai::Dynamic) -> Self {
        let get_f64 = |d: &rhai::Dynamic| {
            d.clone()
                .try_cast::<f64>()
                .or_else(|| d.clone().try_cast::<i64>().map(|f| f as f64))
        };

        if let Some(v) = get_f64(&d) {
            Value::Float(v)
        } else if let Some(v) = d.clone().try_cast::<Vec2>() {
            Value::Vec2(v)
        } else if let Some(v) = d.clone().try_cast::<Vec3>() {
            Value::Vec3(v)
        } else if let Some(v) = d.clone().try_cast::<Tree>() {
            Value::Tree(v)
        } else if let Some(v) = d.clone().try_cast::<String>() {
            Value::String(v)
        } else if let Some(arr) = d.clone().into_array().ok().and_then(|arr| {
            arr.iter().map(get_f64).collect::<Option<Vec<f64>>>()
        }) {
            match arr.len() {
                2 => Value::Vec2(Vec2 {
                    x: arr[0],
                    y: arr[1],
                }),
                3 => Value::Vec3(Vec3 {
                    x: arr[0],
                    y: arr[1],
                    z: arr[2],
                }),
                _ => Value::Dynamic(d),
            }
        } else {
            // TODO handle array
            Value::Dynamic(d)
        }
    }
}

struct Block {
    name: String,
    script: String,
}

#[derive(Copy, Clone, Hash, Eq, PartialEq)]
struct BlockIndex(u64);

impl BlockIndex {
    fn id(&self) -> egui::Id {
        egui::Id::new("block").with(self.0)
    }
}

struct World {
    next_index: u64,
    order: Vec<BlockIndex>,
    blocks: HashMap<BlockIndex, Block>,
}

impl World {
    /// Filters blocks based on a function
    fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&BlockIndex) -> bool,
    {
        self.blocks.retain(|index, _block| f(index));
        self.order.retain(|index| f(index))
    }

    /// Appends a new empty block to the end of the list
    fn new_empty_block(&mut self) {
        let index = BlockIndex(self.next_index);
        self.next_index += 1;
        self.blocks.insert(
            index,
            Block {
                name: "block".to_owned(),
                script: "".to_owned(),
            },
        );
        self.order.push(index);
    }
}

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
        });

        // Customize egui here with cc.egui_ctx.set_fonts and cc.egui_ctx.set_visuals.
        // Restore app state using cc.storage (requires the "persistence" feature).
        // Use the cc.gl (a glow::Context) to create graphics shaders and buffers that you can use
        // for e.g. egui::PaintCallback.
        Self {
            data: World {
                blocks: HashMap::new(),
                order: vec![],
                next_index: 0,
            },
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::SidePanel::left("left_panel")
            .min_width(250.0)
            .show(ctx, |ui| {
                self.left(ui);
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Hello World!");
        });
    }
}

#[derive(Copy, Clone, Default)]
struct NameEdit {
    needs_focus: bool,
}

impl App {
    fn left(&mut self, ui: &mut egui::Ui) {
        // Draw blocks
        let mut to_delete = HashSet::new();
        let mut toggle_edit = HashSet::new();
        // XXX there is a drag-and-drop implementation that's built into egui,
        // see `egui_demo_lib/src/demo/drag_and_drop.rs`
        dnd(ui, "dnd").show_vec(
            &mut self.data.order,
            |ui, index, handle, state| match draggable_block(
                ui,
                *index,
                self.data.blocks.get_mut(index).unwrap(),
                handle,
                state,
            ) {
                Some(BlockResponse::Delete) => {
                    to_delete.insert(*index);
                }
                Some(BlockResponse::Edit) => {
                    toggle_edit.insert(*index);
                }
                None => (),
            },
        );

        // Post-processing: edit blocks based on button presses
        self.data.retain(|index| !to_delete.contains(index));

        // Draw the "new block" button below a separator
        if !self.data.blocks.is_empty() {
            ui.separator();
        }
        if ui.button(NEW_BLOCK).clicked() {
            self.data.new_empty_block();
        }
    }
}

enum BlockResponse {
    Delete,
    Edit,
}

/// Draws a draggable block within a [`egui_dnd`] context
///
/// Returns a [`BlockResponse`] based on button presses
#[must_use]
fn draggable_block(
    ui: &mut egui::Ui,
    index: BlockIndex,
    block: &mut Block,
    handle: egui_dnd::Handle,
    state: egui_dnd::ItemState,
) -> Option<BlockResponse> {
    let id = index.id();
    let mut response = None;
    egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        false,
    )
    .show_header(ui, |ui| {
        // Editable object name
        block_name(ui, index, block);
        // Buttons on the left side
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.add_space(5.0);
                handle.show_drag_cursor_on_hover(false).ui(ui, |ui| {
                    ui.add(egui::Button::new(DRAG).selected(state.dragged));
                });
                if ui.button(TRASH).clicked() {
                    response = Some(BlockResponse::Delete);
                }
                if ui.button(PENCIL).clicked() {
                    response = Some(BlockResponse::Edit);
                }
            },
        );
    })
    .body_unindented(|ui| ui.text_edit_multiline(&mut block.script));
    response
}

/// Draws the name of a block, editable with a double-click
fn block_name(ui: &mut egui::Ui, index: BlockIndex, block: &mut Block) {
    let id = index.id();
    match ui.memory(|mem| mem.data.get_temp(id)) {
        Some(NameEdit { needs_focus }) => {
            let response = ui.add(
                egui::TextEdit::singleline(&mut block.name)
                    .desired_width(ui.available_width() / 2.0),
            );
            let lost_focus = response.lost_focus();
            ui.memory_mut(|mem| {
                if needs_focus {
                    mem.request_focus(response.id);
                    mem.data.insert_temp(id, NameEdit { needs_focus: false });
                }
                if lost_focus {
                    mem.data.remove_temp::<NameEdit>(id);
                }
            });
        }
        None => {
            let response = ui
                .scope_builder(
                    egui::UiBuilder::new().sense(egui::Sense::click()),
                    |ui| ui.heading(&block.name),
                )
                .response;
            if response.double_clicked() {
                ui.memory_mut(|mem| {
                    mem.data.insert_temp(id, NameEdit { needs_focus: true })
                });
            }
        }
    }
}

const NEW_BLOCK: &str = "\u{f067} New block";
const DRAG: &str = "\u{f0041}";
const TRASH: &str = "\u{f48e}";
const PENCIL: &str = "\u{f03eb}";

const INCONSOLATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fonts/InconsolataNerdFontPropo-Regular.ttf"
));
