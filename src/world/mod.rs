use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use fidget::context::Tree;

pub use crate::state::BlockIndex;
use crate::state::{BlockState, WorldState};
use heck::ToSnakeCase;

mod scene;
mod shapes;
pub use scene::Scene;
pub use shapes::ShapeLibrary;

pub struct Block {
    pub name: String,
    pub script: String,

    pub data: Option<BlockData>,

    /// Map from input name to expression
    ///
    /// This does not live in the [`BlockData`] because it must be persistent;
    /// the resulting values _are_ stored in the block state.
    pub inputs: HashMap<String, String>,
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

impl From<&Block> for BlockState {
    fn from(b: &Block) -> BlockState {
        BlockState {
            name: b.name.clone(),
            script: b.script.clone(),
            inputs: b.inputs.clone(),
        }
    }
}

impl Block {
    /// Checks whether the block is error-free
    ///
    /// A block with no state is _invalid_, i.e. returns `false`
    pub fn is_valid(&self) -> bool {
        self.data.as_ref().is_some_and(|s| s.error.is_none())
    }

    /// Gets the `BlockView`, if the block is free of errors
    pub fn get_view(&self) -> Option<&BlockView> {
        self.data.as_ref().and_then(|s| s.view.as_ref())
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
pub enum BlockError {
    NameError(NameError),
    EvalError(EvalError),
}

#[derive(Clone)]
pub struct EvalError {
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
    Input(Result<rhai::Dynamic, String>),
    Output { value: rhai::Dynamic, text: String },
}

pub struct BlockView {
    pub scene: scene::Scene,
}

/// Transient block data (e.g. evaluation results)
///
/// This data is _not_ saved or serialized; it can be recalculated on-demand
/// from the world's state.
pub struct BlockData {
    /// Output from `print` calls in the script
    pub stdout: String,
    /// Output from `debug` calls in the script, pinned to specific lines
    pub debug: HashMap<usize, Vec<String>>,
    /// Error encountered when evaluating the script
    pub error: Option<BlockError>,
    /// Values defined with `input(..)` or `output(..)` calls in the script
    pub io_values: Vec<(String, IoValue)>,
    /// Value exported to a view
    pub view: Option<BlockView>,
}

impl From<&World> for WorldState {
    /// Returns a version of the world without transient state
    ///
    /// (used for serialization and evaluation)
    fn from(w: &World) -> Self {
        WorldState {
            next_index: w.next_index,
            order: w.order.clone(),
            blocks: w.blocks.iter().map(|(k, v)| (*k, v.into())).collect(),
        }
    }
}

impl From<WorldState> for World {
    /// Rebuilds the entire world, populating [`BlockData`] for each block
    fn from(state: WorldState) -> Self {
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
}

impl PartialEq<WorldState> for World {
    fn eq(&self, other: &WorldState) -> bool {
        self.next_index == other.next_index
            && self.order == other.order
            && self.blocks.len() == other.blocks.len()
            && self.blocks.iter().all(|(i, b)| {
                let Some(other) = other.blocks.get(i) else {
                    return false;
                };
                b.name == other.name
                    && b.script == other.script
                    && b.inputs == other.inputs
            })
    }
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
    pub fn new_block_from(&mut self, s: &shapes::ShapeDefinition) -> bool {
        let index = BlockIndex::new(self.next_index);
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
            error: None,
            debug: HashMap::new(),
            io_values: vec![],
            view: None,
        });
        let data = block.data.as_mut().unwrap();

        let mut engine = fidget::rhai::engine();
        let ast = match engine.compile(&block.script) {
            Ok(ast) => ast,
            Err(e) => {
                data.error = Some(BlockError::EvalError(EvalError {
                    message: e.to_string(),
                    line: e.position().line(),
                }));
                return input_scope;
            }
        };

        // Build the data used during block evaluation
        let eval_data = Arc::new(RwLock::new(BlockEvalData::new(
            std::mem::take(&mut block.inputs),
            input_scope,
        )));
        BlockEvalData::bind(&eval_data, &mut engine);

        let r = engine.eval_ast::<rhai::Dynamic>(&ast);

        // Update block state based on actions taken by the script
        let eval_data = std::mem::take(&mut *eval_data.write().unwrap());
        data.stdout = eval_data.stdout.join("\n");
        data.debug = eval_data.debug;
        data.io_values = eval_data.values;
        data.view = eval_data.view.map(|scene| BlockView { scene });

        // Update inputs, which may have been modified
        block.inputs = eval_data.inputs;

        if let Err(e) = r {
            data.error = Some(BlockError::EvalError(EvalError {
                message: e.to_string(),
                line: e.position().line(),
            }));
        } else {
            // If the script evaluated successfully, filter out any input
            // fields which haven't been used in the script.
            block.inputs.retain(|k, _| eval_data.new_inputs.contains(k));
        }

        // Then, check whether we can bind outputs to the block name.  We'll
        // first check that the name is valid.  We prioritize script errors over
        // name errors, so will not replace an existing value in `data.error`
        let mut input_scope = eval_data.scope;
        if !rhai::is_valid_identifier(&block.name) {
            if data.error.is_none() {
                data.error =
                    Some(BlockError::NameError(NameError::InvalidIdentifier));
            }
            return input_scope;
        }

        // Next, bind from the name to a block index (if available)
        match name_map.entry(block.name.clone()) {
            std::collections::hash_map::Entry::Occupied(..) => {
                if data.error.is_none() {
                    data.error =
                        Some(BlockError::NameError(NameError::DuplicateName));
                }
                return input_scope;
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(i);
            }
        }

        // Write IO values into the shared input scope.  The value which is
        // written depends on a few heuristics:
        // - If there is a single output, then write it with the object name
        // - Otherwise, write both outputs and inputs as an object map
        let mut output_values = vec![];
        let mut input_values = vec![];
        for (name, value) in &data.io_values {
            match value {
                IoValue::Output { value, .. } => {
                    output_values.push((name.into(), value.clone()));
                }
                IoValue::Input(Ok(value)) => {
                    input_values.push((name.into(), value.clone()))
                }
                IoValue::Input(Err(..)) => (),
            }
        }
        if output_values.len() == 1 {
            let (_name, value) = output_values.pop().unwrap();
            input_scope.push(&block.name, value.clone());
            // Automatically add a View if there's a single tree output
            if let Some(tree) = value.try_cast::<Tree>() {
                if data.view.is_none() {
                    data.view = Some(BlockView { scene: tree.into() })
                }
            }
        } else {
            let obj: rhai::Map = output_values
                .into_iter()
                .chain(input_values)
                .map(|(n, v)| (n, v))
                .collect();
            input_scope.push(&block.name, obj);
        }

        input_scope
    }
}

/// Handle to intermediate block data during evaluation
#[derive(Default)]
struct BlockEvalData {
    names: HashSet<String>,
    values: Vec<(String, IoValue)>,
    view: Option<scene::Scene>,

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
        let e = ctx.engine();
        let mut scope = rhai::Scope::new();
        scope.push("v", v.clone());
        let text = e.eval_with_scope(&mut scope, "to_string(v)").unwrap();
        self.values
            .push((name.to_owned(), IoValue::Output { value: v, text }));
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
            Ok(value) => Ok(value.clone()),
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
        self.view = Some(tree.into());
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
