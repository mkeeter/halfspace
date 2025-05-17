use std::collections::HashMap;

use fidget::{
    context::Tree,
    shapes::{Vec2, Vec3, Vec4},
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

struct Block {
    /// Map from input name to expression
    inputs: HashMap<String, String>,

    /// Map from output name to value
    outputs: HashMap<String, Value>,

    /// Script to be evaluated
    script: String,
}

enum Object {
    Block(Block),
    Group(String, Vec<Object>),
}

struct NamedObject {
    name: String,
    object: Object,

    /// Is the name being actively edited?
    name_edit: Option<String>,
}

struct World {
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
        cc.egui_ctx.all_styles_mut(|style| {
            style.interaction.selectable_labels = false;
        });
        // Customize egui here with cc.egui_ctx.set_fonts and cc.egui_ctx.set_visuals.
        // Restore app state using cc.storage (requires the "persistence" feature).
        // Use the cc.gl (a glow::Context) to create graphics shaders and buffers that you can use
        // for e.g. egui::PaintCallback.
        Self {
            data: World { data: vec![] },
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
    fn left(&mut self, ui: &mut egui::Ui) {
        ui.heading("Left Column");
        for obj in &mut self.data.data {
            // Editable object name
            match &mut obj.name_edit {
                None => {
                    let response = ui
                        .scope_builder(
                            egui::UiBuilder::new().sense(egui::Sense::click()),
                            |ui| ui.heading(&obj.name),
                        )
                        .response;
                    if response.double_clicked() {
                        obj.name_edit = Some(obj.name.clone());
                    }
                }
                Some(name) => {
                    if ui.text_edit_singleline(name).lost_focus() {
                        obj.name = std::mem::take(name);
                        obj.name_edit = None;
                    }
                }
            };
            match &mut obj.object {
                Object::Block(b) => {
                    ui.text_edit_multiline(&mut b.script);
                }
                Object::Group(_name, _data) => (),
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Data").clicked() {
                self.data.data.push(NamedObject {
                    name: "HI".to_owned(),
                    object: Object::Block(Block {
                        inputs: HashMap::new(),
                        outputs: HashMap::new(),
                        script: "OMG WTF".to_owned(),
                    }),
                    name_edit: None,
                })
            }
        });
    }
}
