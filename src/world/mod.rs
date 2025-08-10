use log::warn;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use fidget::context::Tree;

pub use crate::state::BlockIndex;
use crate::state::{BlockState, ScriptState, ValueState, WorldState};
use facet::Facet;
use heck::ToSnakeCase;

mod scene;
mod shapes;
pub use scene::{Color, Drawable, Scene};
pub use shapes::{ShapeKind, ShapeLibrary};

#[allow(clippy::large_enum_variant)]
pub enum Block {
    Script(ScriptBlock),
    Value(ValueBlock),
}

impl Block {
    pub fn name(&self) -> &str {
        match self {
            Block::Script(s) => s.name.as_str(),
            Block::Value(s) => s.name.as_str(),
        }
    }

    pub fn has_view(&self) -> bool {
        self.get_view().is_some()
    }

    /// Checks whether the block is error-free
    ///
    /// A block with no state is _invalid_, i.e. returns `false`
    pub fn is_valid(&self) -> bool {
        match self {
            Block::Script(s) => {
                s.data.as_ref().is_some_and(|s| s.error.is_none())
            }
            Block::Value(s) => {
                s.data.as_ref().is_some_and(|s| s.output.is_ok())
            }
        }
    }

    /// Gets the `BlockView`, if the block is free of errors
    pub fn get_view(&self) -> Option<&BlockView> {
        match self {
            Block::Script(s) => s.data.as_ref().and_then(|s| s.view.as_ref()),
            Block::Value(s) => s.data.as_ref().and_then(|s| s.view.as_ref()),
        }
    }
}

pub struct ScriptBlock {
    pub name: String,
    pub script: String,

    pub data: Option<ScriptData>,

    /// Map from input name to expression
    ///
    /// This does not live in the [`ScriptData`] because it must be persistent;
    /// the resulting values _are_ stored in the block state.
    pub inputs: HashMap<String, String>,
}

pub struct ValueBlock {
    pub name: String,
    pub input: String,
    pub data: Option<ValueData>,
}

impl From<BlockState> for Block {
    fn from(b: BlockState) -> Self {
        match b {
            BlockState::Script(b) => Self::Script(ScriptBlock {
                name: b.name,
                script: b.script,
                inputs: b.inputs,
                data: None,
            }),
            BlockState::Value(b) => Self::Value(ValueBlock {
                name: b.name,
                input: b.input,
                data: None,
            }),
        }
    }
}

impl From<&Block> for BlockState {
    fn from(b: &Block) -> BlockState {
        match b {
            Block::Script(b) => BlockState::Script(ScriptState {
                name: b.name.clone(),
                script: b.script.clone(),
                inputs: b.inputs.clone(),
            }),
            Block::Value(b) => BlockState::Value(ValueState {
                name: b.name.clone(),
                input: b.input.clone(),
            }),
        }
    }
}

#[derive(Default)]
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

#[derive(Debug, thiserror::Error)]
pub enum BlockError {
    #[error(transparent)]
    Name(#[from] NameError),
    #[error(transparent)]
    Parse(#[from] rhai::ParseError),
    #[error(transparent)]
    Eval(#[from] Box<rhai::EvalAltResult>),
}

impl BlockError {
    /// Prints the error chain
    pub fn print_chain(&self) -> String {
        let mut e = self as &dyn std::error::Error;
        let mut chain = format!("{e}");
        while let Some(source) = e.source() {
            chain += &format!(": {source}");
            e = source;
        }
        chain
    }
}

#[derive(Copy, Clone, Debug, thiserror::Error)]
pub enum NameError {
    #[error("invalid identifier")]
    InvalidIdentifier,
    #[error("duplicate name")]
    DuplicateName,
}

#[derive(Clone)]
pub enum IoValue {
    Input {
        pos: rhai::Position,
        value: Result<rhai::Dynamic, String>,
    },
    Output {
        pos: rhai::Position,
        value: rhai::Dynamic,
        text: String,
    },
}

impl IoValue {
    fn pos(&self) -> rhai::Position {
        match self {
            IoValue::Input { pos, .. } | IoValue::Output { pos, .. } => *pos,
        }
    }
}

pub struct BlockView {
    pub scene: scene::Scene,
}

/// Transient script data (e.g. evaluation results)
///
/// This data is _not_ saved or serialized; it can be recalculated on-demand
/// from the world's state.
pub struct ScriptData {
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
    /// Export request from the script
    pub export: Option<ExportRequest>,
}

/// Transient value data (e.g. evaluation results)
///
/// This data is _not_ saved or serialized; it can be recalculated on-demand
/// from the world's state.
pub struct ValueData {
    /// Single output value
    pub output: Result<rhai::Dynamic, BlockError>,
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
    /// Rebuilds the entire world, populating data for each block
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
                match (b, other) {
                    (Block::Script(b), BlockState::Script(other)) => {
                        b.name == other.name
                            && b.script == other.script
                            && b.inputs == other.inputs
                    }
                    (Block::Value(b), BlockState::Value(other)) => {
                        b.name == other.name && b.input == other.input
                    }
                    _ => false,
                }
            })
    }
}

impl World {
    /// Builds a new (empty) world
    pub fn new() -> Self {
        Self::default()
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
            .map(|b| b.name())
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

        let b = match &s.kind {
            ShapeKind::Script { inputs, script } => {
                // Special casing: if the shape has a single tree input and our
                // last block has a single tree output or input, then we
                // pre-populate the input.
                let mut iter = inputs.iter().filter(|(_name, i)| {
                    i.ty.is_some_and(|ty| {
                        ty == Tree::SHAPE.id || ty == Vec::<Tree>::SHAPE.id
                    }) && i.text.is_empty()
                });
                let tree_input = iter.next().filter(|_| iter.next().is_none());
                let mut last_tree = None;
                if let Some(i) = self.order.last()
                    && let Block::Script(ScriptBlock {
                        name,
                        data: Some(data),
                        ..
                    }) = &self.blocks[i]
                {
                    let mut output_count = 0;
                    let mut input_count = 0;
                    let mut has_tree = false;
                    for (_name, i) in data.io_values.iter() {
                        has_tree |= match i {
                            IoValue::Output { value, .. } => {
                                output_count += 1;
                                value
                                    .clone()
                                    .try_cast::<fidget::context::Tree>()
                                    .is_some()
                            }
                            IoValue::Input { value, .. } => {
                                input_count += 1;
                                value.as_ref().is_ok_and(|v| {
                                    v.clone()
                                        .try_cast::<fidget::context::Tree>()
                                        .is_some()
                                })
                            }
                        }
                    }
                    if has_tree && ((output_count == 1) ^ (input_count == 1)) {
                        last_tree = Some(name);
                    }
                }
                let mut inputs = inputs
                    .iter()
                    .map(|(name, v)| (name.clone(), v.text.clone()))
                    .collect::<HashMap<_, _>>();
                if let Some(tree_input) = tree_input
                    && let Some(last_tree) = last_tree
                {
                    *inputs.get_mut(tree_input.0).unwrap() =
                        last_tree.to_owned();
                }

                Block::Script(ScriptBlock {
                    name,
                    script: script.clone(),
                    inputs,
                    data: None,
                })
            }
            ShapeKind::Value { input } => Block::Value(ValueBlock {
                name,
                input: input.clone(),
                data: None,
            }),
        };

        self.blocks.insert(index, b);
        self.order.push(index);
        true
    }

    fn rebuild(&mut self) {
        // Steal order so that we can mutate self; we'll swap it back later
        let order = std::mem::take(&mut self.order);

        // Inputs are evaluated in a separate context with a scope that's
        // accumulated from previous block outputs.
        let mut input_scope = rhai::Scope::new();

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
        match block {
            Block::Script(s) => {
                Self::rebuild_script_block(i, s, input_scope, name_map)
            }
            Block::Value(s) => {
                Self::rebuild_value_block(i, s, input_scope, name_map)
            }
        }
    }

    fn rebuild_script_block(
        i: BlockIndex,
        block: &mut ScriptBlock,
        input_scope: rhai::Scope<'static>,
        name_map: &mut HashMap<String, BlockIndex>,
    ) -> rhai::Scope<'static> {
        block.data = Some(ScriptData {
            stdout: String::new(),
            error: None,
            debug: HashMap::new(),
            io_values: vec![],
            view: None,
            export: None,
        });
        let data = block.data.as_mut().unwrap();

        let mut engine = fidget::rhai::engine();
        scene::register_types(&mut engine); // add scene and drawable types
        let ast = match engine.compile(&block.script) {
            Ok(ast) => ast,
            Err(e) => {
                data.error = Some(BlockError::Parse(e));
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

        // Update block state based on actions taken by the script.  We manually
        // unpack `data` here to produce a compiler error if it changes.
        let eval_data = std::mem::take(&mut *eval_data.write().unwrap());
        let ScriptData {
            stdout,
            debug,
            io_values,
            view,
            error,
            export,
        } = data;
        *stdout = eval_data.stdout.join("\n");
        *debug = eval_data.debug;
        *io_values = eval_data.values;
        *view = eval_data.view.map(|scene| BlockView { scene });
        *export = eval_data.export;

        // Update inputs, which may have been modified
        block.inputs = eval_data.inputs;

        if let Err(e) = r {
            *error = Some(BlockError::Eval(e));
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
                    Some(BlockError::Name(NameError::InvalidIdentifier));
            }
            return input_scope;
        }

        // Next, bind from the name to a block index (if available)
        match name_map.entry(block.name.clone()) {
            std::collections::hash_map::Entry::Occupied(..) => {
                if data.error.is_none() {
                    data.error =
                        Some(BlockError::Name(NameError::DuplicateName));
                }
                return input_scope;
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(i);
            }
        }

        // Write IO values into the shared input scope.  The value which is
        // written depends on a few heuristics:
        // - If there is a single output or input, then write it with the object
        //   name
        // - Otherwise, write both outputs and inputs as an object map
        let mut output_values = vec![];
        let mut input_values = vec![];
        for (name, value) in &data.io_values {
            match value {
                IoValue::Output { value, .. } => {
                    output_values.push((name.into(), value.clone()));
                }
                IoValue::Input {
                    value: Ok(value), ..
                } => input_values.push((name.into(), value.clone())),
                IoValue::Input { value: Err(..), .. } => (),
            }
        }
        let single_value = if output_values.len() == 1 {
            let (_name, value) = output_values.pop().unwrap();
            Some(value)
        } else if input_values.len() == 1 && output_values.is_empty() {
            let (_name, value) = input_values.pop().unwrap();
            Some(value)
        } else {
            let obj: rhai::Map =
                output_values.into_iter().chain(input_values).collect();
            input_scope.push(&block.name, obj);
            None
        };

        // Handle the special case of a single input (or output) value
        if data.error.is_none()
            && let Some(value) = single_value
        {
            input_scope.push(&block.name, value.clone());
            // If there's no view but there's a single view-compatible output,
            // then treat it as the view.
            if data.view.is_none() {
                if let Some(tree) = value.clone().try_cast::<Tree>() {
                    data.view = Some(BlockView { scene: tree.into() })
                } else if let Some(d) = value.clone().try_cast::<Drawable>() {
                    data.view = Some(BlockView { scene: d.into() })
                } else if let Some(scene) = value.try_cast::<Scene>() {
                    data.view = Some(BlockView { scene })
                }
            }
        }

        input_scope
    }

    fn rebuild_value_block(
        i: BlockIndex,
        block: &mut ValueBlock,
        input_scope: rhai::Scope<'static>,
        name_map: &mut HashMap<String, BlockIndex>,
    ) -> rhai::Scope<'static> {
        let mut engine = fidget::rhai::engine();
        scene::register_types(&mut engine); // add scene and drawable types
        let ast = match engine.compile(&block.input) {
            Ok(ast) => ast,
            Err(e) => {
                block.data = Some(ValueData {
                    output: Err(BlockError::Parse(e)),
                    view: None,
                });
                return input_scope;
            }
        };

        // Build the data used during block evaluation
        let eval_data = Arc::new(RwLock::new(BlockEvalData::new(
            HashMap::new(), // no inputs for value blocks
            input_scope,
        )));
        // Note that we don't call `BlockEvalData::bind` here, because we're
        // only evaluating a single expression.

        // TODO check for single expression?
        let r = engine.eval_ast::<rhai::Dynamic>(&ast);

        // Update block state based on actions taken by the script
        let eval_data = std::mem::take(&mut *eval_data.write().unwrap());
        block.data = Some(ValueData {
            output: r.map_err(BlockError::Eval),
            view: eval_data.view.map(|scene| BlockView { scene }),
        });

        // Then, check whether we can bind outputs to the block name.  We'll
        // first check that the name is valid.  We prioritize script errors over
        // name errors, so will not replace an existing value in `data.error`
        let mut input_scope = eval_data.scope;
        let data = block.data.as_mut().unwrap();
        if !rhai::is_valid_identifier(&block.name) {
            if data.output.is_ok() {
                data.output =
                    Err(BlockError::Name(NameError::InvalidIdentifier));
            }
            return input_scope;
        }

        // Next, bind from the name to a block index (if available)
        match name_map.entry(block.name.clone()) {
            std::collections::hash_map::Entry::Occupied(..) => {
                if data.output.is_ok() {
                    data.output =
                        Err(BlockError::Name(NameError::DuplicateName));
                }
                return input_scope;
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(i);
            }
        }

        if let Ok(value) = &data.output {
            input_scope.push(&block.name, value.clone());
            // If there's a single view-compatible output, then treat it as the
            // view.
            if data.view.is_none() {
                if let Some(tree) = value.clone().try_cast::<Tree>() {
                    data.view = Some(BlockView { scene: tree.into() })
                } else if let Some(d) = value.clone().try_cast::<Drawable>() {
                    data.view = Some(BlockView { scene: d.into() })
                } else if let Some(scene) = value.clone().try_cast::<Scene>() {
                    data.view = Some(BlockView { scene })
                }
            }
        }

        input_scope
    }

    pub fn import_data(&mut self, mut other: World) {
        for (i, b) in self.blocks.iter_mut() {
            let Some(ob) = other.blocks.remove(i) else {
                continue;
            };
            match (b, ob) {
                (Block::Script(b), Block::Script(ob)) => {
                    let new_data = ob.data.unwrap();
                    if new_data.error.is_none() {
                        // If the new block evaluated successfully, then we
                        // replace everything.
                        b.data = Some(new_data);

                        // Delete old inputs; create new inputs (but do not edit
                        // text for pre-existing shared inputs, because it may
                        // have been changed while the world was evaluated
                        // off-thread)
                        b.inputs.retain(|k, _| ob.inputs.contains_key(k));
                        for (k, i) in ob.inputs {
                            b.inputs.entry(k).or_insert(i);
                        }
                    } else if let Some(prev_data) = b.data.as_mut() {
                        // We have pre-existing old data, so create new outputs
                        // and update their values, but do not delete old ones.
                        // n.b. we manually unpack the ScriptData object here,
                        // so we get a compiler error when it changes
                        let ScriptData {
                            stdout,
                            debug,
                            error,
                            view,
                            io_values,
                            export,
                        } = prev_data;
                        *stdout = new_data.stdout;
                        *debug = new_data.debug;
                        *error = new_data.error;
                        *view = new_data.view;
                        *export = new_data.export;

                        let mut nv = new_data
                            .io_values
                            .into_iter()
                            .collect::<HashMap<_, _>>();
                        for (s, v) in io_values.iter_mut() {
                            if let Some(n) = nv.remove(s) {
                                *v = n;
                            } else if let IoValue::Output {
                                value, text, ..
                            } = v
                            {
                                // Previous outputs are marked as invalid but
                                // stay in the GUI, to avoid jitter
                                *value = rhai::Dynamic::from(());
                                *text = "[evaluation failed]".to_string();
                            }
                        }

                        // Merge remaining IO values based on textual position
                        let mut nv = nv.into_iter().collect::<Vec<_>>();
                        nv.sort_by_key(|(_name, v)| v.pos());
                        let mut ia =
                            std::mem::take(io_values).into_iter().peekable();
                        let mut ib = nv.into_iter().peekable();
                        let mut new_order: Vec<(String, IoValue)> = vec![];
                        loop {
                            match (ia.peek(), ib.peek()) {
                                (Some(va), Some(vb)) => {
                                    if va.1.pos() < vb.1.pos() {
                                        new_order.push(ia.next().unwrap());
                                    } else {
                                        new_order.push(ib.next().unwrap());
                                    }
                                }
                                (Some(..), None) => {
                                    new_order.push(ia.next().unwrap())
                                }
                                (None, Some(..)) => {
                                    new_order.push(ib.next().unwrap())
                                }
                                (None, None) => break,
                            }
                        }
                        *io_values = new_order;

                        // Create new inputs, but do not delete old ones or edit
                        // text for pre-existing shared inputs.
                        for (k, i) in ob.inputs {
                            b.inputs.entry(k).or_insert(i);
                        }
                    } else {
                        // If we have no old data, then replace everything
                        b.data = Some(new_data);

                        // Create new inputs, but do not delete old ones or edit
                        // text for pre-existing shared inputs.
                        for (k, i) in ob.inputs {
                            b.inputs.entry(k).or_insert(i);
                        }
                    }
                }
                (Block::Value(b), Block::Value(ob)) => {
                    b.data = ob.data;
                }
                _ => warn!("cannot import data from different block types"),
            }
        }
    }
}

#[derive(Clone)]
pub enum ExportRequest {
    Mesh {
        tree: fidget::context::Tree,
        min: fidget::shapes::types::Vec3,
        max: fidget::shapes::types::Vec3,
        feature_size: f64,
    },
    Image {
        scene: Scene,
        min: fidget::shapes::types::Vec2,
        max: fidget::shapes::types::Vec2,
        resolution: f64,
    },
}

/// Handle to intermediate block data during evaluation
#[derive(Default)]
struct BlockEvalData {
    names: HashSet<String>,
    values: Vec<(String, IoValue)>,
    view: Option<scene::Scene>,
    export: Option<ExportRequest>,

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
            export: None,
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
                format!("io `{name}` already exists"),
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
        self.values.push((
            name.to_owned(),
            IoValue::Output {
                value: v,
                text,
                pos: ctx.position(),
            },
        ));
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
        self.values.push((
            name.to_owned(),
            IoValue::Input {
                value: i,
                pos: ctx.position(),
            },
        ));
        v.map_err(|_| "error in input expression".into())
    }

    fn view<T: Into<Scene>>(
        &mut self,
        ctx: rhai::NativeCallContext,
        t: T,
    ) -> Result<(), Box<rhai::EvalAltResult>> {
        if self.view.is_some() {
            return Err(rhai::EvalAltResult::ErrorRuntime(
                "cannot have multiple views in a single block".into(),
                ctx.position(),
            )
            .into());
        }
        self.view = Some(t.into());
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
        let eval_data_ = eval_data.clone();
        engine.register_fn(
            "view",
            move |ctx: rhai::NativeCallContext,
                  scene: Scene|
                  -> Result<(), Box<rhai::EvalAltResult>> {
                let mut eval_data = eval_data_.write().unwrap();
                eval_data.view(ctx, scene)
            },
        );
        let eval_data_ = eval_data.clone();
        engine.register_fn(
            "view",
            move |ctx: rhai::NativeCallContext,
                  draw: Drawable|
                  -> Result<(), Box<rhai::EvalAltResult>> {
                let mut eval_data = eval_data_.write().unwrap();
                eval_data.view(ctx, draw)
            },
        );

        let eval_data_ = eval_data.clone();
        engine.register_fn(
            "export_mesh",
            move |ctx: rhai::NativeCallContext,
                  tree: fidget::context::Tree,
                  min: fidget::shapes::types::Vec3,
                  max: fidget::shapes::types::Vec3,
                  feature_size: f64|
                  -> Result<(), Box<rhai::EvalAltResult>> {
                let mut eval_data = eval_data_.write().unwrap();
                if eval_data.export.is_some() {
                    return Err(rhai::EvalAltResult::ErrorRuntime(
                        "cannot have multiple exports in a single block".into(),
                        ctx.position(),
                    )
                    .into());
                }
                eval_data.export = Some(ExportRequest::Mesh {
                    tree,
                    min,
                    max,
                    feature_size,
                });
                Ok(())
            },
        );
        let eval_data_ = eval_data.clone();
        engine.register_fn(
            "export_image",
            move |ctx: rhai::NativeCallContext,
                  scene: Scene,
                  min: fidget::shapes::types::Vec2,
                  max: fidget::shapes::types::Vec2,
                  resolution: f64|
                  -> Result<(), Box<rhai::EvalAltResult>> {
                let mut eval_data = eval_data_.write().unwrap();
                if eval_data.export.is_some() {
                    return Err(rhai::EvalAltResult::ErrorRuntime(
                        "cannot have multiple exports in a single block".into(),
                        ctx.position(),
                    )
                    .into());
                }
                eval_data.export = Some(ExportRequest::Image {
                    scene,
                    min,
                    max,
                    resolution,
                });
                Ok(())
            },
        );
    }
}
