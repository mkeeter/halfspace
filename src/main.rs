use egui_dnd::dnd;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use fidget::{
    context::Tree,
    shapes::{Vec2, Vec3},
};

#[allow(unused)]
#[derive(Clone)]
enum Value {
    Float(f64),
    Vec2(Vec2),
    Vec3(Vec3),
    Tree(Tree),
    String(String),
    Dynamic(rhai::Dynamic),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Float(v) => write!(f, "{v}"),
            Value::Vec2(v) => write!(f, "vec2({}, {})", v.x, v.y),
            Value::Vec3(v) => write!(f, "vec3({}, {}, {})", v.x, v.y, v.z),
            Value::Tree(_) => write!(f, "Tree(..)"),
            Value::String(s) => write!(f, "\"{s}\""),
            Value::Dynamic(d) => write!(f, "{d}"),
        }
    }
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

impl Value {
    fn to_dynamic(&self) -> rhai::Dynamic {
        match self {
            Value::Vec2(v) => rhai::Dynamic::from(*v),
            Value::Vec3(v) => rhai::Dynamic::from(*v),
            Value::Float(v) => rhai::Dynamic::from(*v),
            Value::Tree(v) => rhai::Dynamic::from(v.clone()),
            Value::String(v) => rhai::Dynamic::from(v.clone()),
            Value::Dynamic(v) => v.clone(),
        }
    }
}

struct Block {
    name: String,
    script: String,
    state: Option<BlockState>,

    /// Map from input name to expression
    ///
    /// This does not live in the `BlockState` because it must be persistent;
    /// the resulting values _are_ stored in the block state.
    inputs: HashMap<String, String>,
}

impl Block {
    fn without_state(&self) -> Self {
        Self {
            name: self.name.clone(),
            script: self.script.clone(),
            inputs: self.inputs.clone(),
            state: None,
        }
    }
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

#[derive(Clone)]
struct BlockError {
    #[allow(unused)] // TODO
    line: Option<usize>,
    message: String,
}

#[derive(Copy, Clone)]
enum NameError {
    InvalidIdentifier,
    DuplicateName,
}

enum IoValue {
    Input(Result<Value, String>),
    Output(Value),
}

struct BlockState {
    /// Output from `print` calls in the script
    stdout: String,
    /// Output from `debug` calls in the script, pinned to specific lines
    debug: HashMap<usize, Vec<String>>,
    /// Error encountered evaluating the name
    name_error: Option<NameError>,
    /// Errors encountered while parsing and evaluating the script
    script_errors: Vec<BlockError>,
    /// Values defined with `input(..)` or `output(..)` calls in the script
    io_values: Vec<(String, IoValue)>,
}

impl World {
    /// Filters blocks based on a function
    ///
    /// Returns `true` if anything changed, or `false` otherwise
    #[must_use]
    fn retain<F>(&mut self, mut f: F) -> bool
    where
        F: FnMut(&BlockIndex) -> bool,
    {
        let prev_len = self.order.len();
        self.blocks.retain(|index, _block| f(index));
        self.order.retain(|index| f(index));
        self.order.len() != prev_len
    }

    /// Appends a new empty block to the end of the list
    ///
    /// Returns `true` if anything changed (which is always the case)
    #[must_use]
    fn new_empty_block(&mut self) -> bool {
        let index = BlockIndex(self.next_index);
        self.next_index += 1;
        let names = self
            .blocks
            .values()
            .map(|b| b.name.as_str())
            .collect::<HashSet<_>>();
        // XXX this is Accidentally Quadratic if you add a bunch of blocks
        let name = std::iter::once("block".to_owned())
            .chain((0..).map(|i| format!("block_{i:03}")))
            .find(|name| !names.contains(name.as_str()))
            .unwrap();

        self.blocks.insert(
            index,
            Block {
                name,
                script: "".to_owned(),
                inputs: HashMap::new(),
                state: None,
            },
        );
        self.order.push(index);
        true
    }

    /// Returns a copy without `BlockState`, suitable for re-evaluation
    fn without_state(&self) -> Self {
        Self {
            next_index: self.next_index,
            order: self.order.clone(),
            blocks: self
                .blocks
                .iter()
                .map(|(k, v)| (*k, v.without_state()))
                .collect(),
        }
    }

    /// Rebuilds the entire world, populating [`BlockState`] for each block
    fn rebuild(&mut self) {
        let mut name_map = HashMap::new();
        let mut engine = rhai::Engine::new();
        engine.set_fail_on_invalid_map_property(true);

        // Bind IO handlers to the engine's `print` and `debug` calls
        #[derive(Default)]
        struct IoLog {
            stdout: Vec<String>,
            debug: HashMap<usize, Vec<String>>,
        }
        impl IoLog {
            fn take(&mut self) -> (Vec<String>, HashMap<usize, Vec<String>>) {
                (
                    std::mem::take(&mut self.stdout),
                    std::mem::take(&mut self.debug),
                )
            }
        }
        let io_log = Arc::new(RwLock::new(IoLog::default()));
        let io_debug = io_log.clone();
        engine.on_debug(move |x, _src, pos| {
            io_debug
                .write()
                .unwrap()
                .debug
                .entry(pos.line().unwrap())
                .or_default()
                .push(x.to_owned())
        });
        let io_print = io_log.clone();
        engine.on_print(move |s| {
            io_print.write().unwrap().stdout.push(s.to_owned())
        });

        // Bind a custom `output` function
        #[derive(Default)]
        struct IoValues {
            names: HashSet<String>,
            values: Vec<(String, IoValue)>,
        }
        impl IoValues {
            fn take(&mut self) -> (HashSet<String>, Vec<(String, IoValue)>) {
                (
                    std::mem::take(&mut self.names),
                    std::mem::take(&mut self.values),
                )
            }
        }
        let io_values = Arc::new(RwLock::new(IoValues::default()));
        let output_handle = io_values.clone();
        engine.register_fn(
            "output",
            move |ctx: rhai::NativeCallContext,
                  name: &str,
                  v: rhai::Dynamic|
                  -> Result<(), Box<rhai::EvalAltResult>> {
                let mut out = output_handle.write().unwrap();
                if !rhai::is_valid_identifier(name) {
                    return Err(rhai::EvalAltResult::ErrorForbiddenVariable(
                        name.to_owned(),
                        ctx.position(),
                    )
                    .into());
                } else if !out.names.insert(name.to_owned()) {
                    return Err(rhai::EvalAltResult::ErrorVariableExists(
                        format!("io `{}` already exists", name),
                        ctx.position(),
                    )
                    .into());
                }
                out.values
                    .push((name.to_owned(), IoValue::Output(Value::from(v))));
                Ok(())
            },
        );

        // Inputs are evaluated in a separate context with a scope that's
        // accumulated from previous block outputs.
        let input_scope = Arc::new(RwLock::new(rhai::Scope::new()));

        for i in &self.order {
            let block = &mut self.blocks.get_mut(i).unwrap();
            block.state = Some(BlockState {
                stdout: String::new(),
                name_error: None,
                debug: HashMap::new(),
                script_errors: vec![],
                io_values: vec![],
            });
            let state = block.state.as_mut().unwrap();

            // Check that the name is valid
            if !rhai::is_valid_identifier(&block.name) {
                state.name_error = Some(NameError::InvalidIdentifier);
                continue;
            }
            // Bind from the name to a block index (if available)
            match name_map.entry(block.name.clone()) {
                std::collections::hash_map::Entry::Occupied(..) => {
                    state.name_error = Some(NameError::DuplicateName);
                    continue;
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(*i);
                }
            }

            let input_scope_handle = input_scope.clone();
            let input_text = Arc::new(RwLock::new(block.inputs.clone()));
            let input_text_handle = input_text.clone();
            let input_handle = io_values.clone();
            let input_fn = move |ctx: rhai::NativeCallContext,
                                 name: &str|
                  -> Result<
                rhai::Dynamic,
                Box<rhai::EvalAltResult>,
            > {
                let mut input_handle = input_handle.write().unwrap();
                if !rhai::is_valid_identifier(name) {
                    return Err(rhai::EvalAltResult::ErrorForbiddenVariable(
                        name.to_owned(),
                        ctx.position(),
                    )
                    .into());
                } else if !input_handle.names.insert(name.to_owned()) {
                    return Err(rhai::EvalAltResult::ErrorVariableExists(
                        format!("io `{}` already exists", name),
                        ctx.position(),
                    )
                    .into());
                }
                let mut input_text_lock = input_text_handle.write().unwrap();
                let txt = input_text_lock
                    .entry(name.to_owned())
                    .or_insert("0".to_owned());
                let e = ctx.engine();
                let mut scope = input_scope_handle.write().unwrap();
                let v = e.eval_expression_with_scope::<rhai::Dynamic>(
                    &mut scope, txt,
                );
                let i = match &v {
                    Ok(value) => Ok(Value::from(value.clone())),
                    Err(e) => Err(e.to_string()),
                };
                input_handle
                    .values
                    .push((name.to_owned(), IoValue::Input(i)));
                v.map_err(|_| "error in input expression".into())
            };
            engine.register_fn("input", input_fn);

            let ast = match engine.compile(&block.script) {
                Ok(ast) => ast,
                Err(e) => {
                    state.script_errors.push(BlockError {
                        message: e.to_string(),
                        line: e.position().line(),
                    });
                    continue;
                }
            };
            let mut scope = rhai::Scope::new();
            let r =
                engine.eval_ast_with_scope::<rhai::Dynamic>(&mut scope, &ast);
            let (stdout, debug) = io_log.write().unwrap().take();
            state.stdout = stdout.join("\n");
            state.debug = debug;
            let (io_names, io_values) = io_values.write().unwrap().take();
            state.io_values = io_values;
            block.inputs = std::mem::take(&mut input_text.write().unwrap());

            // Write outputs into the shared input scope
            let obj: rhai::Map = state
                .io_values
                .iter()
                .filter_map(|(name, value)| match value {
                    IoValue::Output(value) => {
                        Some((name.into(), value.to_dynamic()))
                    }
                    IoValue::Input(..) => None,
                })
                .collect();
            input_scope.write().unwrap().push(&block.name, obj);

            if let Err(e) = r {
                state.script_errors.push(BlockError {
                    message: e.to_string(),
                    line: e.position().line(),
                });
            } else {
                // If the script evaluated successfully, filter out any input
                // fields which haven't been used in the script.
                block.inputs.retain(|k, _| io_names.contains(k));
            }
        }
    }
}

struct BoundWorld<'a> {
    world: &'a mut World,
    syntax: &'a egui_extras::syntax_highlighting::SyntectSettings,
    changed: &'a mut bool,
}

impl<'a> egui_dock::TabViewer for BoundWorld<'a> {
    type Tab = BlockIndex;

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        tab.id()
    }

    fn title(&mut self, index: &mut BlockIndex) -> egui::WidgetText {
        egui::WidgetText::from(&self.world.blocks[index].name)
    }

    /// Draw a block as as editable text pane
    fn ui(&mut self, ui: &mut egui::Ui, index: &mut BlockIndex) {
        let block = self.world.blocks.get_mut(index).unwrap();
        let theme =
            egui_extras::syntax_highlighting::CodeTheme::from_style(ui.style());
        let mut layouter = |ui: &egui::Ui, buf: &str, wrap_width: f32| {
            let mut layout_job =
                egui_extras::syntax_highlighting::highlight_with(
                    ui.ctx(),
                    ui.style(),
                    &theme,
                    buf,
                    "rhai",
                    self.syntax,
                );
            layout_job.wrap.max_width = wrap_width;
            ui.fonts(|f| f.layout_job(layout_job))
        };
        let r = ui.add(
            egui::TextEdit::multiline(&mut block.script)
                .font(egui::TextStyle::Monospace) // for cursor height
                .code_editor()
                .desired_rows(10)
                .lock_focus(true)
                .desired_width(f32::INFINITY)
                .layouter(&mut layouter),
        );
        *self.changed |= r.changed();
        if let Some(state) = &mut block.state {
            if !state.stdout.is_empty() {
                ui.label("Output");
                ui.add(
                    egui::TextEdit::multiline(&mut state.stdout)
                        .interactive(false)
                        .desired_width(f32::INFINITY),
                );
            }
            if !state.script_errors.is_empty() {
                ui.label("Errors");
                let mut text = state
                    .script_errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.scope(|ui| {
                    let vis = ui.visuals_mut();
                    vis.widgets.inactive = vis.widgets.active;
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .interactive(false)
                            .desired_width(f32::INFINITY),
                    );
                });
            }
        }
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
            data: World {
                blocks: HashMap::new(),
                order: vec![],
                next_index: 0,
            },
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

                let mut bw = BoundWorld {
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

#[derive(Copy, Clone, Default)]
struct NameEdit {
    needs_focus: bool,
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
                let r = draggable_block(
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
        if !self.data.blocks.is_empty() {
            ui.separator();
        }
        if ui.button(NEW_BLOCK).clicked() {
            changed |= self.data.new_empty_block();
        }
        changed
    }
}

// The `bitflags!` macro generates `struct`s that manage a set of flags.
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

/// Draws a draggable block within a [`egui_dnd`] context
///
/// Returns a [`BlockResponse`] based on button presses
fn draggable_block(
    ui: &mut egui::Ui,
    index: BlockIndex,
    block: &mut Block,
    is_open: bool,
    last: bool,
    handle: egui_dnd::Handle,
    state: egui_dnd::ItemState,
) -> BlockResponse {
    let mut response = BlockResponse::empty();
    let padding = ui.spacing().icon_width + ui.spacing().icon_spacing;
    if block
        .state
        .as_ref()
        .is_some_and(|s| !s.io_values.is_empty())
    {
        egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            index.id(),
            true,
        )
        .show_header(ui, |ui| {
            response =
                draggable_block_header(ui, index, block, is_open, handle, state)
        })
        .body_unindented(|ui| {
            let state = block.state.as_ref().unwrap();
            for (name, value) in &state.io_values {
                ui.horizontal(|ui| {
                    ui.add_space(padding);
                    ui.label(name);
                    match value {
                        IoValue::Output(value) => {
                            let mut txt = value.to_string();
                            ui.add_enabled(
                                false,
                                egui::TextEdit::singleline(&mut txt)
                                    .desired_width(f32::INFINITY),
                            );
                        }
                        IoValue::Input(value) => {
                            // TODO show errors here?
                            let s = block.inputs.get_mut(name).unwrap();
                            if ui
                                .add(
                                    egui::TextEdit::singleline(s)
                                        .desired_width(f32::INFINITY),
                                )
                                .changed()
                            {
                                response |= BlockResponse::CHANGED;
                            }
                        }
                    }
                });
            }
            if !last {
                ui.separator();
            }
        });
    } else {
        ui.horizontal(|ui| {
            ui.add_space(padding);
            response =
                draggable_block_header(ui, index, block, is_open, handle, state)
        });
    }
    response
}

fn draggable_block_header(
    ui: &mut egui::Ui,
    index: BlockIndex,
    block: &mut Block,
    is_open: bool,
    handle: egui_dnd::Handle,
    state: egui_dnd::ItemState,
) -> BlockResponse {
    // Editable object name
    let mut response = BlockResponse::empty();
    if block_name(ui, index, block) {
        response |= BlockResponse::CHANGED;
    }
    // Buttons on the left side
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(5.0);
        handle.show_drag_cursor_on_hover(false).ui(ui, |ui| {
            ui.add(egui::Button::new(DRAG).selected(state.dragged));
        });
        if ui.button(TRASH).clicked() {
            response |= BlockResponse::DELETE;
        }
        if ui
            .add(egui::Button::new(PENCIL).selected(is_open))
            .clicked()
        {
            response = BlockResponse::TOGGLE_EDIT;
        }
        if let Some(state) = &block.state {
            if let Some(e) = state.name_error {
                let err = match e {
                    NameError::DuplicateName => "duplicate name",
                    NameError::InvalidIdentifier => "invalid identifier",
                };
                ui.label(
                    egui::RichText::new(WARN)
                        .color(ui.style().visuals.error_fg_color),
                )
                .on_hover_ui(|ui| {
                    ui.label(err);
                });
            } else if !state.script_errors.is_empty() {
                let r = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(WARN)
                                .color(ui.style().visuals.warn_fg_color),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_ui(|ui| {
                        ui.label("script contains error");
                    });
                if r.clicked() {
                    response |= BlockResponse::FOCUS_ERR;
                }
            }
        }
    });
    response
}

/// Draws the name of a block, editable with a double-click
///
/// Returns `true` if the name has changed, `false` otherwise
fn block_name(ui: &mut egui::Ui, index: BlockIndex, block: &mut Block) -> bool {
    let id = index.id();
    let mut changed = false;
    match ui.memory(|mem| mem.data.get_temp(id)) {
        Some(NameEdit { needs_focus }) => {
            let response = ui.add(
                egui::TextEdit::singleline(&mut block.name)
                    // XXX fix width
                    .desired_width(ui.available_width() / 2.0),
            );
            let lost_focus = response.lost_focus();
            changed |= response.changed();
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
            let (enabled, name) = if block.name.is_empty() {
                (false, "[empty]")
            } else {
                (true, block.name.as_str())
            };
            let response = ui
                .scope_builder(
                    egui::UiBuilder::new().sense(egui::Sense::click()),
                    |ui| ui.add_enabled(enabled, egui::Label::new(name)),
                )
                .response;
            if response.double_clicked() {
                ui.memory_mut(|mem| {
                    mem.data.insert_temp(id, NameEdit { needs_focus: true })
                });
            }
        }
    }
    changed
}

// Unicode symbols from Nerd Fonts, see https://www.nerdfonts.com/cheat-sheet
const NEW_BLOCK: &str = "\u{f067} New block";
const DRAG: &str = "\u{f0041}";
const TRASH: &str = "\u{f48e}";
const PENCIL: &str = "\u{f03eb}";
const WARN: &str = "\u{f071}";

const INCONSOLATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fonts/InconsolataNerdFontPropo-Regular.ttf"
));
const SYNTAX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/syntax.bin"));
