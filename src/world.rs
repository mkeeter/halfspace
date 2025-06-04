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
pub enum Value {
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
            // TODO handle array of integers?
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

pub struct Block {
    pub name: String,
    pub script: String,
    pub state: Option<BlockState>,

    /// Map from input name to expression
    ///
    /// This does not live in the `BlockState` because it must be persistent;
    /// the resulting values _are_ stored in the block state.
    pub inputs: HashMap<String, String>,
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

    /// Checks whether the block is error-free
    ///
    /// A block with no state is _invalid_, i.e. returns `false`
    pub fn is_valid(&self) -> bool {
        self.state.as_ref().is_some_and(|s| {
            s.name_error.is_none() && s.script_errors.is_empty()
        })
    }

    /// Gets the `BlockView`, if the block is free of errors
    pub fn get_view(&self) -> Option<&BlockView> {
        self.state
            .as_ref()
            .filter(|s| s.name_error.is_none() && s.script_errors.is_empty())
            .and_then(|s| s.view.as_ref())
    }
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct BlockIndex(u64);

impl BlockIndex {
    pub fn id(&self) -> egui::Id {
        egui::Id::new("block").with(self.0)
    }
}

pub struct World {
    next_index: u64,
    pub order: Vec<BlockIndex>,
    pub blocks: HashMap<BlockIndex, Block>,
}

impl std::ops::Index<BlockIndex> for World {
    type Output = Block;
    fn index(&self, index: BlockIndex) -> &Self::Output {
        &self.blocks[&index]
    }
}

impl std::ops::IndexMut<BlockIndex> for World {
    fn index_mut(&mut self, index: BlockIndex) -> &mut Self::Output {
        self.blocks.get_mut(&index).unwrap()
    }
}

#[derive(Clone)]
pub struct BlockError {
    #[allow(unused)] // TODO
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Copy, Clone)]
pub enum NameError {
    InvalidIdentifier,
    DuplicateName,
}

pub enum IoValue {
    Input(Result<Value, String>),
    Output(Value),
}

pub struct BlockView {
    pub tree: fidget::context::Tree,
}

pub struct BlockState {
    /// Output from `print` calls in the script
    pub stdout: String,
    /// Output from `debug` calls in the script, pinned to specific lines
    pub debug: HashMap<usize, Vec<String>>,
    /// Error encountered evaluating the name
    pub name_error: Option<NameError>,
    /// Errors encountered while parsing and evaluating the script
    pub script_errors: Vec<BlockError>,
    /// Values defined with `input(..)` or `output(..)` calls in the script
    pub io_values: Vec<(String, IoValue)>,
    /// Value exported to a view
    pub view: Option<BlockView>,
}

impl World {
    /// Builds a new (empty) world
    pub fn new() -> Self {
        World {
            blocks: HashMap::new(),
            order: vec![],
            next_index: 0,
        }
    }

    /// Checks whether the world is empty
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Filters blocks based on a function
    ///
    /// Returns `true` if anything changed, or `false` otherwise
    #[must_use]
    pub fn retain<F>(&mut self, mut f: F) -> bool
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
    pub fn new_empty_block(&mut self) -> bool {
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
    pub fn without_state(&self) -> Self {
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
    pub fn rebuild(&mut self) {
        let mut name_map = HashMap::new();
        let mut engine = rhai::Engine::new();
        fidget::rhai::tree::register(&mut engine);
        fidget::rhai::vec::register(&mut engine);
        fidget::rhai::shapes::register(&mut engine);
        engine.register_fn("axes", || -> rhai::Array {
            let (x, y, z) = fidget::context::Tree::axes();
            vec![
                rhai::Dynamic::from(x),
                rhai::Dynamic::from(y),
                rhai::Dynamic::from(z),
            ]
        });
        engine.set_fail_on_invalid_map_property(true);
        engine.set_max_expr_depths(64, 32);
        engine.on_progress(move |count| {
            // Pick a number, any number
            if count > 50_000 {
                Some("script runtime exceeded".into())
            } else {
                None
            }
        });

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

        let io_values = Arc::new(RwLock::new(IoValues::default()));
        let output_handle = io_values.clone();
        engine.register_fn(
            "output",
            move |ctx: rhai::NativeCallContext,
                  name: &str,
                  v: rhai::Dynamic|
                  -> Result<(), Box<rhai::EvalAltResult>> {
                let mut out = output_handle.write().unwrap();
                out.insert_name(&ctx, name)?;
                out.values
                    .push((name.to_owned(), IoValue::Output(Value::from(v))));
                Ok(())
            },
        );

        // Inputs are evaluated in a separate context with a scope that's
        // accumulated from previous block outputs.
        let input_scope = Arc::new(RwLock::new(rhai::Scope::new()));
        {
            let mut i = input_scope.write().unwrap();
            i.push_constant("x", fidget::context::Tree::x());
            i.push_constant("y", fidget::context::Tree::y());
            i.push_constant("z", fidget::context::Tree::z());
        }

        for i in &self.order {
            let block = &mut self.blocks.get_mut(i).unwrap();
            block.state = Some(BlockState {
                stdout: String::new(),
                name_error: None,
                debug: HashMap::new(),
                script_errors: vec![],
                io_values: vec![],
                view: None,
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
                                 name: rhai::Dynamic|
                  -> Result<
                rhai::Dynamic,
                Box<rhai::EvalAltResult>,
            > {
                let name = if let Ok(c) = name.as_char() {
                    format!("{c}")
                } else {
                    name.into_string()?
                };
                let mut input_handle = input_handle.write().unwrap();
                input_handle.insert_name(&ctx, &name)?;

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
            let view_handle = io_values.clone();
            engine.register_fn(
                "view",
                move |ctx: rhai::NativeCallContext,
                      tree: fidget::context::Tree|
                      -> Result<(), Box<rhai::EvalAltResult>> {
                    let mut view_handle = view_handle.write().unwrap();
                    if view_handle.view.is_some() {
                        return Err(rhai::EvalAltResult::ErrorRuntime(
                            "cannot have multiple views in a single block"
                                .into(),
                            ctx.position(),
                        )
                        .into());
                    }
                    view_handle.view = Some(tree);
                    Ok(())
                },
            );

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
            scope.push_constant("x", fidget::context::Tree::x());
            scope.push_constant("y", fidget::context::Tree::y());
            scope.push_constant("z", fidget::context::Tree::z());
            let r =
                engine.eval_ast_with_scope::<rhai::Dynamic>(&mut scope, &ast);

            // Update block state based on actions taken by the script
            let (stdout, debug) = io_log.write().unwrap().take();
            state.stdout = stdout.join("\n");
            state.debug = debug;
            let (io_names, io_values, io_view) =
                io_values.write().unwrap().take();
            state.io_values = io_values;
            state.view = io_view.map(|tree| BlockView { tree });

            // Update inputs, which may have been modified
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

/// Helper `struct` to accumulate `print` and `debug` calls
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

/// Helper struct to record `input`, `output`, and `view` calls
#[derive(Default)]
struct IoValues {
    names: HashSet<String>,
    values: Vec<(String, IoValue)>,
    view: Option<fidget::context::Tree>,
}

impl IoValues {
    fn insert_name(
        &mut self,
        ctx: &rhai::NativeCallContext,
        name: &str,
    ) -> Result<(), Box<rhai::EvalAltResult>> {
        if !rhai::is_valid_identifier(name) {
            Err(rhai::EvalAltResult::ErrorForbiddenVariable(
                name.to_owned(),
                ctx.position(),
            )
            .into())
        } else if !self.names.insert(name.to_owned()) {
            Err(rhai::EvalAltResult::ErrorVariableExists(
                format!("io `{}` already exists", name),
                ctx.position(),
            )
            .into())
        } else {
            Ok(())
        }
    }
    fn take(
        &mut self,
    ) -> (
        HashSet<String>,
        Vec<(String, IoValue)>,
        Option<fidget::context::Tree>,
    ) {
        (
            std::mem::take(&mut self.names),
            std::mem::take(&mut self.values),
            self.view.take(),
        )
    }
}
