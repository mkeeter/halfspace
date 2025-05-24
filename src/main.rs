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

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
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

impl egui_dock::TabViewer for World {
    type Tab = BlockIndex;

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        tab.id()
    }

    fn title(&mut self, index: &mut BlockIndex) -> egui::WidgetText {
        egui::WidgetText::from(&self.blocks[index].name)
    }

    fn ui(&mut self, ui: &mut egui::Ui, index: &mut BlockIndex) {
        let block = self.blocks.get_mut(index).unwrap();
        let theme =
            egui_extras::syntax_highlighting::CodeTheme::from_style(ui.style());
        let mut layouter = |ui: &egui::Ui, buf: &str, wrap_width: f32| {
            let mut layout_job = egui_extras::syntax_highlighting::highlight(
                ui.ctx(),
                ui.style(),
                &theme,
                buf,
                "rs",
            );
            layout_job.wrap.max_width = wrap_width;
            ui.fonts(|f| f.layout_job(layout_job))
        };
        ui.add(
            egui::TextEdit::multiline(&mut block.script)
                .font(egui::TextStyle::Monospace) // for cursor height
                .code_editor()
                .desired_rows(10)
                .lock_focus(true)
                .desired_width(f32::INFINITY)
                .layouter(&mut layouter),
        );
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
    tree: egui_dock::DockState<BlockIndex>,
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
            style.text_styles.insert(
                egui::TextStyle::Monospace,
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
            tree: egui_dock::DockState::new(vec![]),
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

                egui_dock::DockArea::new(&mut self.tree)
                    .style(egui_dock::Style::from_egui(ctx.style().as_ref()))
                    .show_leaf_collapse_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .show_inside(ui, &mut self.data);
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
        // XXX there is a drag-and-drop implementation that's built into egui,
        // see `egui_demo_lib/src/demo/drag_and_drop.rs`
        dnd(ui, "dnd").show_vec(
            &mut self.data.order,
            |ui, index, handle, state| {
                let tab_location = self.tree.find_tab(index);
                match draggable_block(
                    ui,
                    *index,
                    self.data.blocks.get_mut(index).unwrap(),
                    tab_location.is_some(),
                    handle,
                    state,
                ) {
                    Some(BlockResponse::Delete) => {
                        to_delete.insert(*index);
                    }
                    Some(BlockResponse::Edit) => {
                        if let Some(tab_location) = tab_location {
                            self.tree.remove_tab(tab_location).unwrap();
                        } else {
                            self.tree.push_to_focused_leaf(*index);
                        }
                    }
                    None => (),
                }
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
    is_open: bool,
    handle: egui_dnd::Handle,
    state: egui_dnd::ItemState,
) -> Option<BlockResponse> {
    let mut response = None;
    egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        index.id(),
        true,
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
                if ui
                    .add(egui::Button::new(PENCIL).selected(is_open))
                    .clicked()
                {
                    response = Some(BlockResponse::Edit);
                }
            },
        );
    })
    .body(|ui| ui.add_enabled(false, egui::Label::new("no io")));
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
