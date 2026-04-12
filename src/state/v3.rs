//! Major version 3 of serializable state
//!
//! This is identical to `v2` except that 2D debug rendering is removed.
//!
//! Forward compatibility must be maintained!
use super::{MigrateFrom, ReadData, v2};
use serde::{Deserialize, Serialize};

pub const MAJOR_VERSION: usize = 3;
pub const MINOR_VERSION: usize = 0;

pub use v2::{
    BlockState, Metadata, ScriptState, Tab, TabMode, ValueState, ViewMode3,
    WorldState,
};

pub struct Reader;
impl super::Reader for Reader {
    type Tab = Tab;
    type WorldState = WorldState;
    type Metadata = Metadata;
    type ViewState = ViewState;
    const MAJOR_VERSION: usize = MAJOR_VERSION;
    const MINOR_VERSION: usize = MINOR_VERSION;
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
        #[serde(default)]
        perspective: bool,
    },
}

/// Available modes for a 2D view in the GUI
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ViewMode2 {
    Sdf,
    Bitfield,
}

impl From<v2::ViewMode2> for ViewMode2 {
    fn from(v: v2::ViewMode2) -> Self {
        match v {
            v2::ViewMode2::Debug => ViewMode2::Sdf,
            v2::ViewMode2::Sdf => ViewMode2::Sdf,
            v2::ViewMode2::Bitfield => ViewMode2::Bitfield,
        }
    }
}

impl MigrateFrom<v2::Reader> for Reader {
    fn migrate(r: ReadData<v2::Reader>) -> ReadData<Self> {
        ReadData {
            world: r.world,
            meta: r.meta,
            views: r.views.into_iter().map(|(i, b)| (i, b.into())).collect(),
            dock: r.dock,
        }
    }
}

impl From<v2::ViewState> for ViewState {
    fn from(v: v2::ViewState) -> Self {
        match v {
            v2::ViewState::View2 {
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
            v2::ViewState::View3 {
                mode,
                center,
                scale,
                width,
                height,
                pitch,
                yaw,
                depth,
                perspective,
            } => ViewState::View3 {
                mode,
                center,
                scale,
                width,
                height,
                pitch,
                yaw,
                depth,
                perspective,
            },
        }
    }
}
