use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use fidget::{
    context::Tree,
    shapes::{Vec2, Vec3},
};
use heck::ToSnakeCase;
use serde::{Deserialize, Serialize};

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

    pub data: Option<BlockData>,

    /// Map from input name to expression
    ///
    /// This does not live in the `BlockData` because it must be persistent;
    /// the resulting values _are_ stored in the block state.
    pub inputs: HashMap<String, String>,
}

/// Serialization-friendly subset of block state
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockState {
    name: String,
    script: String,
    inputs: HashMap<String, String>,
}

impl From<BlockState> for Block {
    fn from(value: BlockState) -> Self {
        Self {
            name: value.name,
            script: value.script,
            inputs: value.inputs,
            data: None,
        }
    }
}

impl Block {
    fn state(&self) -> BlockState {
        BlockState {
            name: self.name.clone(),
            script: self.script.clone(),
            inputs: self.inputs.clone(),
        }
    }

    /// Checks whether the block is error-free
    ///
    /// A block with no state is _invalid_, i.e. returns `false`
    pub fn is_valid(&self) -> bool {
        self.data.as_ref().is_some_and(|s| {
            s.name_error.is_none() && s.script_errors.is_empty()
        })
    }

    /// Gets the `BlockView`, if the block is free of errors
    pub fn get_view(&self) -> Option<&BlockView> {
        self.data
            .as_ref()
            .filter(|s| s.name_error.is_none() && s.script_errors.is_empty())
            .and_then(|s| s.view.as_ref())
    }
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
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

/// Serialization-friendly subset of world state
///
/// This is identical to [`World`], but with [`Block`] replaced with
/// [`BlockState`] (to avoid the un-serializable [`BlockData`])
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    next_index: u64,
    pub order: Vec<BlockIndex>,
    pub blocks: HashMap<BlockIndex, BlockState>,
}

impl From<WorldState> for World {
    fn from(value: WorldState) -> Self {
        Self {
            next_index: value.next_index,
            order: value.order,
            blocks: value
                .blocks
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
        }
    }
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

#[derive(Clone)]
pub enum IoValue {
    Input(Result<Value, String>),
    Output(Value),
}

#[derive(Clone)]
pub struct BlockView {
    pub tree: fidget::context::Tree,
}

/// Transient block data (e.g. evaluation results)
///
/// This data is _not_ saved or serialized; it can be recalculated on-demand
/// from the world's state.
#[derive(Clone)]
pub struct BlockData {
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

    fn next_name_with_prefix(&self, s: &str) -> String {
        // XXX this is Accidentally Quadratic if you add a bunch of blocks
        let names = self
            .blocks
            .values()
            .map(|b| b.name.as_str())
            .collect::<HashSet<_>>();

        std::iter::once(s.to_owned())
            .chain((0..).map(|i| format!("{s}_{i:03}")))
            .find(|name| !names.contains(name.as_str()))
            .unwrap()
    }

    #[must_use]
    pub fn new_block_from(
        &mut self,
        s: &crate::shapes::ShapeDefinition,
    ) -> bool {
        let index = BlockIndex(self.next_index);
        self.next_index += 1;
        let name = self.next_name_with_prefix(&s.name.to_snake_case());
        self.blocks.insert(
            index,
            Block {
                name,
                script: s.script.clone(),
                inputs: s.inputs.clone(),
                data: None,
            },
        );
        self.order.push(index);
        true
    }

    /// Returns a version of the world without transient state
    ///
    /// (used for serialization and evaluation)
    pub fn state(&self) -> WorldState {
        WorldState {
            next_index: self.next_index,
            order: self.order.clone(),
            blocks: self.blocks.iter().map(|(k, v)| (*k, v.state())).collect(),
        }
    }

    /// Rebuilds the entire world, populating [`BlockData`] for each block
    pub fn build_from_state(state: WorldState) -> Self {
        let mut world = World {
            next_index: state.next_index,
            order: state.order,
            blocks: state
                .blocks
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
        };
        world.rebuild();
        world
    }

    fn rebuild(&mut self) {
        // Steal order so that we can mutate self; we'll swap it back later
        let order = std::mem::take(&mut self.order);

        // Inputs are evaluated in a separate context with a scope that's
        // accumulated from previous block outputs.
        let mut input_scope = rhai::Scope::new();
        input_scope.push_constant("x", fidget::context::Tree::x());
        input_scope.push_constant("y", fidget::context::Tree::y());
        input_scope.push_constant("z", fidget::context::Tree::z());

        // We maintain a separate map of block names to detect duplicates
        let mut name_map = HashMap::new();
        for i in &order {
            input_scope = self.rebuild_block(*i, input_scope, &mut name_map);
        }
        self.order = order;
    }

    fn rebuild_block(
        &mut self,
        i: BlockIndex,
        input_scope: rhai::Scope<'static>,
        name_map: &mut HashMap<String, BlockIndex>,
    ) -> rhai::Scope<'static> {
        let block = self.blocks.get_mut(&i).unwrap();
        block.data = Some(BlockData {
            stdout: String::new(),
            name_error: None,
            debug: HashMap::new(),
            script_errors: vec![],
            io_values: vec![],
            view: None,
        });
        let data = block.data.as_mut().unwrap();

        // Check that the name is valid
        if !rhai::is_valid_identifier(&block.name) {
            data.name_error = Some(NameError::InvalidIdentifier);
            return input_scope;
        }
        // Bind from the name to a block index (if available)
        match name_map.entry(block.name.clone()) {
            std::collections::hash_map::Entry::Occupied(..) => {
                data.name_error = Some(NameError::DuplicateName);
                return input_scope;
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(i);
            }
        }

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

        let ast = match engine.compile(&block.script) {
            Ok(ast) => ast,
            Err(e) => {
                data.script_errors.push(BlockError {
                    message: e.to_string(),
                    line: e.position().line(),
                });
                return input_scope;
            }
        };

        // Build the data used during block evaluation
        let eval_data = Arc::new(RwLock::new(BlockEvalData::new(
            std::mem::take(&mut block.inputs),
            input_scope,
        )));
        BlockEvalData::bind(&eval_data, &mut engine);

        // Local scope for evaluating the body of the script
        let mut scope = rhai::Scope::new();
        scope.push_constant("x", fidget::context::Tree::x());
        scope.push_constant("y", fidget::context::Tree::y());
        scope.push_constant("z", fidget::context::Tree::z());
        let r = engine.eval_ast_with_scope::<rhai::Dynamic>(&mut scope, &ast);

        // Update block state based on actions taken by the script
        let eval_data = std::mem::take(&mut *eval_data.write().unwrap());
        data.stdout = eval_data.stdout.join("\n");
        data.debug = eval_data.debug;
        data.io_values = eval_data.values;
        data.view = eval_data.view.map(|tree| BlockView { tree });

        // Update inputs, which may have been modified
        block.inputs = eval_data.inputs;

        // Write outputs into the shared input scope
        let obj: rhai::Map = data
            .io_values
            .iter()
            .filter_map(|(name, value)| match value {
                IoValue::Output(value) => {
                    Some((name.into(), value.to_dynamic()))
                }
                IoValue::Input(..) => None,
            })
            .collect();
        let mut input_scope = eval_data.scope;
        input_scope.push(&block.name, obj);

        if let Err(e) = r {
            data.script_errors.push(BlockError {
                message: e.to_string(),
                line: e.position().line(),
            });
        } else {
            // If the script evaluated successfully, filter out any input
            // fields which haven't been used in the script.
            block.inputs.retain(|k, _| eval_data.new_inputs.contains(k));
        }

        input_scope
    }
}

/// Handle to intermediate block data during evaluation
#[derive(Default)]
struct BlockEvalData {
    names: HashSet<String>,
    values: Vec<(String, IoValue)>,
    view: Option<fidget::context::Tree>,

    stdout: Vec<String>,
    debug: HashMap<usize, Vec<String>>,
    inputs: HashMap<String, String>,
    new_inputs: HashSet<String>,
    scope: rhai::Scope<'static>,
}

impl BlockEvalData {
    fn new(
        inputs: HashMap<String, String>,
        scope: rhai::Scope<'static>,
    ) -> Self {
        Self {
            names: HashSet::new(),
            values: vec![],
            view: None,
            stdout: vec![],
            debug: HashMap::new(),
            inputs,
            new_inputs: HashSet::new(),
            scope,
        }
    }

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

    fn output(
        &mut self,
        ctx: rhai::NativeCallContext,
        name: &str,
        v: rhai::Dynamic,
    ) -> Result<(), Box<rhai::EvalAltResult>> {
        self.insert_name(&ctx, name)?;
        self.values
            .push((name.to_owned(), IoValue::Output(Value::from(v))));
        Ok(())
    }

    fn input(
        &mut self,
        ctx: rhai::NativeCallContext,
        name: rhai::Dynamic,
    ) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
        let name = if let Ok(c) = name.as_char() {
            format!("{c}")
        } else {
            name.into_string()?
        };
        self.insert_name(&ctx, &name)?;

        let txt = self.inputs.entry(name.to_owned()).or_insert("0".to_owned());
        self.new_inputs.insert(name.to_owned());
        let e = ctx.engine();
        let v =
            e.eval_expression_with_scope::<rhai::Dynamic>(&mut self.scope, txt);
        let i = match &v {
            Ok(value) => Ok(Value::from(value.clone())),
            Err(e) => Err(e.to_string()),
        };
        self.values.push((name.to_owned(), IoValue::Input(i)));
        v.map_err(|_| "error in input expression".into())
    }

    fn view(
        &mut self,

        ctx: rhai::NativeCallContext,
        tree: fidget::context::Tree,
    ) -> Result<(), Box<rhai::EvalAltResult>> {
        if self.view.is_some() {
            return Err(rhai::EvalAltResult::ErrorRuntime(
                "cannot have multiple views in a single block".into(),
                ctx.position(),
            )
            .into());
        }
        self.view = Some(tree);
        Ok(())
    }

    /// Binds `input`, `output`, `view`, `print`, and `debug`
    fn bind(eval_data: &Arc<RwLock<Self>>, engine: &mut rhai::Engine) {
        let eval_data_ = eval_data.clone();
        engine.on_debug(move |x, _src, pos| {
            eval_data_
                .write()
                .unwrap()
                .debug
                .entry(pos.line().unwrap())
                .or_default()
                .push(x.to_owned())
        });
        let eval_data_ = eval_data.clone();
        engine.on_print(move |s| {
            eval_data_.write().unwrap().stdout.push(s.to_owned())
        });

        let eval_data_ = eval_data.clone();
        engine.register_fn(
            "input",
            move |ctx: rhai::NativeCallContext, name: rhai::Dynamic| {
                let mut eval_data = eval_data_.write().unwrap();
                eval_data.input(ctx, name)
            },
        );

        let eval_data_ = eval_data.clone();
        engine.register_fn(
            "output",
            move |ctx: rhai::NativeCallContext,
                  name: &str,
                  v: rhai::Dynamic|
                  -> Result<(), Box<rhai::EvalAltResult>> {
                eval_data_.write().unwrap().output(ctx, name, v)
            },
        );

        let eval_data_ = eval_data.clone();
        engine.register_fn(
            "view",
            move |ctx: rhai::NativeCallContext,
                  tree: fidget::context::Tree|
                  -> Result<(), Box<rhai::EvalAltResult>> {
                let mut eval_data = eval_data_.write().unwrap();
                eval_data.view(ctx, tree)
            },
        );
    }
}
