use egui_dnd::dnd;

use fidget::{
    context::Tree,
    shapes::{Vec2, Vec3},
};

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

#[derive(Hash)]
struct Block {
    /// Script to be evaluated
    script: String,
}

#[derive(Hash)]
enum Object {
    Block(Block),
    Group(Vec<Object>),
}

#[derive(Hash)]
struct NamedObject {
    name: String,
    object: Object,
    index: u64,

    /// Is the name being actively edited?
    name_edit: Option<(bool, String)>,
    to_delete: bool,
}

struct World {
    next_index: u64,
    data: Vec<NamedObject>,
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
                data: vec![],
                next_index: 0,
            },
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::SidePanel::left("left_panel").show(ctx, |ui| {
            self.left(ui);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Hello World!");
        });
    }
}

impl App {
    fn object_name(obj: &mut NamedObject, ui: &mut egui::Ui) {
        ui.horizontal(|ui| match &mut obj.name_edit {
            None => {
                let response = ui
                    .scope_builder(
                        egui::UiBuilder::new().sense(egui::Sense::click()),
                        |ui| ui.heading(&obj.name),
                    )
                    .response;
                if response.double_clicked() {
                    obj.name_edit = Some((true, obj.name.clone()));
                }
            }
            Some((wants_focus, name)) => {
                let response = ui.add(
                    egui::TextEdit::singleline(name)
                        .desired_width(ui.available_width() / 2.0),
                );
                if std::mem::take(wants_focus) {
                    ui.memory_mut(|mem| mem.request_focus(response.id));
                }
                if response.lost_focus() {
                    obj.name = std::mem::take(name);
                    obj.name_edit = None;
                }
            }
        });
    }

    fn left(&mut self, ui: &mut egui::Ui) {
        ui.heading("Left Column");
        dnd(ui, "dnd").show_vec(
            &mut self.data.data,
            |ui, obj, handle, _state| {
                // Editable object name
                ui.horizontal(|ui| {
                    Self::object_name(obj, ui);
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.add_space(5.0);
                            handle.ui(ui, |ui| {
                                ui.label(DRAG); // drag symbol
                            });
                            if ui.button(TRASH).clicked() {
                                obj.to_delete = true;
                            }
                        },
                    );
                });
                match &mut obj.object {
                    Object::Block(b) => {
                        ui.text_edit_multiline(&mut b.script);
                    }
                    Object::Group(_data) => (),
                }
            },
        );
        self.data.data.retain(|obj| !obj.to_delete);

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(NEW_FILE).clicked() {
                let index = self.data.next_index;
                self.data.next_index += 1;
                self.data.data.push(NamedObject {
                    index,
                    name: "HI".to_owned(),
                    object: Object::Block(Block {
                        script: "OMG WTF".to_owned(),
                    }),
                    name_edit: None,
                    to_delete: false,
                })
            }
            if ui.button(NEW_FOLDER).clicked() {
                let index = self.data.next_index;
                self.data.next_index += 1;
                self.data.data.push(NamedObject {
                    index,
                    name: "group".to_owned(),
                    object: Object::Group(vec![]),
                    name_edit: None,
                    to_delete: false,
                })
            }
        });
    }
}

const NEW_FILE: &str = " \u{ea7f} ";
const DRAG: &str = " \u{f0041} ";
const TRASH: &str = " \u{f48e} ";
const NEW_FOLDER: &str = " \u{ea80} ";

const INCONSOLATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fonts/InconsolataNerdFontPropo-Regular.ttf"
));
