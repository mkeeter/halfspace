//! Major version 1 of serializable state
//!
//! Forward compatibility must be maintained!
use super::BlockIndex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAJOR_VERSION: usize = 1;
pub const MINOR_VERSION: usize = 2;

/// Serialization-friendly subset of world state
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    pub next_index: u64,
    pub order: Vec<BlockIndex>,
    pub blocks: HashMap<BlockIndex, BlockState>,
}

/// Serialization-friendly subset of block state
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockState {
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
