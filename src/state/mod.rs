//! Application state for serialization and undo / redo
//!
//! The application has a single "current" state used for undo / redo, which is
//! defined in the `pub use` below.  This state uses equality checks and is
//! stored in-memory; it does not require serialization.
//!
//! For saving to disk, state must be serializable.  We use a range-based
//! strategy, with major and minor state versions (e.g. [`v1`] below is a major
//! state version).  At all times, there is a canonical `(major, minor)`
//! version.
//!
//! - Each major version must be backwards-compatible with older minor versions.
//!   In other words, we may add new types and variants (bumping the minor
//!   version each time), but cannot remove old types and variants.
//! - Major versions _may_ be compatible with newer minor versions, if no new
//!   types happen to be used.  In this case, we attempt to load the file, but
//!   may return an error.
//! - Major versions are **not** compatible with each other.  When loading data
//!   serialized with an older major version, we must load it using that
//!   version's deserializer, then migrate it forward.
//!
//! There's one exception: we serialize [`egui_dock::DockState`] directly, and
//! don't have control over its internals (because they're within a separate
//! crate).  Within a major version, we _try_ to deserialize it, but return a
//! default state if deserialization fails.
use crate::view::ViewData;
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod undo;
mod v1;
pub use undo::Undo;
pub use v1::*;

/// Unique index for blocks
///
/// This may never change!
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockIndex(u64);

impl BlockIndex {
    pub fn new(i: u64) -> Self {
        Self(i)
    }
    pub fn id(&self) -> egui::Id {
        // XXX should this be somewhere else, to remove the egui dep?
        egui::Id::new("block").with(self.0)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ReadError {
    #[error("io error encountered when reading file")]
    IoError(#[from] std::io::Error),

    #[error("file is not UTF-8")]
    NotUtf8(#[from] std::str::Utf8Error),

    #[error("could not parse JSON")]
    ParseError(#[from] serde_json::Error),

    #[error("bad tag: expected {expected}, got {actual}")]
    BadTag { expected: String, actual: String },

    #[error(
        "file is too new: our version is {expected_major}.{expected_minor}, \
         file's is {actual_major}.{actual_minor}"
    )]
    TooNew {
        expected_major: usize,
        expected_minor: usize,
        actual_major: usize,
        actual_minor: usize,
    },
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Clone, Serialize)]
pub struct AppState {
    tag: String,
    major: usize,
    minor: usize,
    #[serde(default)]
    pub meta: Metadata,
    pub world: WorldState,
    pub views: HashMap<BlockIndex, ViewState>,
    pub dock: egui_dock::DockState<Tab>,
}

const TAG: &str = "halfspace";

impl Default for AppState {
    fn default() -> Self {
        Self {
            tag: TAG.to_owned(),
            major: MAJOR_VERSION,
            minor: MINOR_VERSION,
            meta: Metadata::default(),
            world: WorldState::default(),
            views: HashMap::new(),
            dock: egui_dock::DockState::new(vec![]),
        }
    }
}

impl AppState {
    pub fn new(
        world: &crate::World,
        views: &HashMap<BlockIndex, ViewData>,
        dock: &egui_dock::DockState<Tab>,
        meta: &Metadata,
    ) -> Self {
        let world = world.into();
        let dock = dock.clone();
        let views = views
            .iter()
            .map(|(k, v)| (*k, (&v.canvas).into()))
            .collect();
        Self {
            tag: TAG.to_owned(),
            major: MAJOR_VERSION,
            minor: MINOR_VERSION,
            meta: meta.clone(),
            world,
            views,
            dock,
        }
    }

    pub fn deserialize(s: &str) -> Result<Self, ReadError> {
        let raw: RawAppState = serde_json::from_str(s)?;
        let too_new = || ReadError::TooNew {
            expected_major: MAJOR_VERSION,
            actual_major: raw.major,
            expected_minor: MINOR_VERSION,
            actual_minor: raw.minor,
        };
        if raw.tag != TAG {
            return Err(ReadError::BadTag {
                expected: TAG.to_owned(),
                actual: raw.tag,
            });
        }
        if raw.major > MAJOR_VERSION {
            return Err(too_new());
        }
        let perhaps_too_new = raw.minor > MINOR_VERSION;
        let world: WorldState =
            serde_json::from_value(raw.world).map_err(|e| {
                if perhaps_too_new {
                    too_new()
                } else {
                    ReadError::from(e)
                }
            })?;
        let meta: Metadata = raw
            .meta
            .map(|r| {
                serde_json::from_value(r).map_err(|e| {
                    if perhaps_too_new {
                        too_new()
                    } else {
                        ReadError::from(e)
                    }
                })
            })
            .transpose()?
            .unwrap_or_default();
        let mut views: HashMap<BlockIndex, ViewState> =
            serde_json::from_value(raw.views).map_err(|e| {
                if perhaps_too_new {
                    too_new()
                } else {
                    ReadError::from(e)
                }
            })?;
        let dock = match serde_json::from_value(raw.dock) {
            Ok(v) => v,
            Err(e) => {
                warn!("could not deserialize dock state: {e:?}");
                views = HashMap::new();
                egui_dock::DockState::new(vec![])
            }
        };
        Ok(Self {
            tag: raw.tag,
            major: raw.major,
            minor: raw.minor,
            meta,
            views,
            world,
            dock,
        })
    }

    pub fn serialize(&self) -> String {
        serde_json::to_string_pretty(self).expect("serialization failed")
    }
}

#[derive(Deserialize)]
struct RawAppState {
    tag: String,
    major: usize,
    minor: usize,
    meta: Option<serde_json::Value>,
    world: serde_json::Value,
    views: serde_json::Value,
    dock: serde_json::Value,
}
