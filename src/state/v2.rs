//! Major version 2 of serializable state
//!
//! Forward compatibility must be maintained!
use super::{BlockIndex, MigrateFrom, ReadData, v1};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAJOR_VERSION: usize = 2;
pub const MINOR_VERSION: usize = 0;

pub struct Reader;
impl super::Reader for Reader {
    type Tab = Tab;
    type WorldState = WorldState;
    type Metadata = Metadata;
    type ViewState = ViewState;
    const MAJOR_VERSION: usize = MAJOR_VERSION;
    const MINOR_VERSION: usize = MINOR_VERSION;
}

////////////////////////////////////////////////////////////////////////////////
// Migration zone!

impl MigrateFrom<v1::Reader> for Reader {
    fn migrate(r: ReadData<v1::Reader>) -> ReadData<Self> {
        ReadData {
            world: r.world.into(),
            meta: r.meta.into(),
            views: r.views.into_iter().map(|(i, b)| (i, b.into())).collect(),
            dock: r.dock.map_tabs(|t| t.into()),
        }
    }
}

impl From<v1::WorldState> for WorldState {
    fn from(v: v1::WorldState) -> Self {
        Self {
            next_index: v.next_index,
            order: v.order,
            blocks: v.blocks.into_iter().map(|(i, b)| (i, b.into())).collect(),
        }
    }
}

impl From<v1::BlockState> for BlockState {
    fn from(b: v1::BlockState) -> Self {
        BlockState::Script(ScriptState {
            name: b.name,
            script: b.script,
            inputs: b.inputs,
        })
    }
}

impl From<v1::Metadata> for Metadata {
    fn from(v: v1::Metadata) -> Self {
        Self {
            description: v.description,
            name: v.name,
        }
    }
}

impl From<v1::ViewState> for ViewState {
    fn from(v: v1::ViewState) -> Self {
        match v {
            v1::ViewState::View2 {
                mode,
                center,
                scale,
                width,
                height,
            } => ViewState::View2 {
                mode: mode.into(),
                center,
                scale,
                width,
                height,
            },
            v1::ViewState::View3 {
                mode,
                center,
                scale,
                width,
                height,
                pitch,
                yaw,
                depth,
            } => ViewState::View3 {
                mode: mode.into(),
                center,
                scale,
                width,
                height,
                pitch,
                yaw,
                depth,
            },
        }
    }
}

impl From<v1::ViewMode2> for ViewMode2 {
    fn from(v: v1::ViewMode2) -> Self {
        match v {
            v1::ViewMode2::Debug => ViewMode2::Debug,
            v1::ViewMode2::Sdf => ViewMode2::Sdf,
            v1::ViewMode2::Bitfield => ViewMode2::Bitfield,
        }
    }
}

impl From<v1::ViewMode3> for ViewMode3 {
    fn from(v: v1::ViewMode3) -> Self {
        match v {
            v1::ViewMode3::Shaded => ViewMode3::Shaded,
            v1::ViewMode3::Heightmap => ViewMode3::Heightmap,
        }
    }
}

impl From<&v1::Tab> for Tab {
    fn from(v: &v1::Tab) -> Self {
        Tab {
            index: v.index,
            mode: v.mode.into(),
        }
    }
}

impl From<v1::TabMode> for TabMode {
    fn from(v: v1::TabMode) -> Self {
        match v {
            v1::TabMode::Script => TabMode::Script,
            v1::TabMode::View => TabMode::View,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Metadata associated with the file
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub description: Option<String>,
    pub name: Option<String>,
}

/// Serialization-friendly subset of world state
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    pub next_index: u64,
    pub order: Vec<BlockIndex>,
    pub blocks: HashMap<BlockIndex, BlockState>,
}

/// Serialization-friendly subset of block state
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BlockState {
    Script(ScriptState),
}

/// Serialization-friendly subset of block state
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptState {
    pub name: String,
    pub script: String,
    pub inputs: HashMap<String, String>,
}

/// Serialization-friendly state associated with a view in the GUI
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum ViewState {
    View2 {
        mode: ViewMode2,
        center: nalgebra::Vector2<f32>,
        scale: f32,
        width: u32,
        height: u32,
    },
    View3 {
        mode: ViewMode3,
        center: nalgebra::Vector3<f32>,
        scale: f32,
        pitch: f32,
        yaw: f32,
        width: u32,
        height: u32,
        depth: u32,
    },
}

/// Available modes for a 2D view in the GUI
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ViewMode2 {
    Sdf,
    Bitfield,
    Debug,
}

/// Available modes for a 3D view in the GUI
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ViewMode3 {
    Heightmap,
    Shaded,
}

/// Available modes for a tab in the GUI
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum TabMode {
    Script,
    View,
}

/// Identifier for a tab in the GUI
///
/// Each block may have one tab for each [`TabMode`]; right now, this is one
/// editor and one viewer.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub index: BlockIndex,
    pub mode: TabMode,
}
