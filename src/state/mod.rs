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
mod v2;
pub use undo::Undo;
pub use v2::*;

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
        "bad major version: expected one of {expected_major:?}, \
         got {actual_major}"
    )]
    BadMajorVersion {
        expected_major: &'static [usize],
        actual_major: usize,
    },

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

trait Reader {
    type WorldState: serde::de::DeserializeOwned;
    type Metadata: serde::de::DeserializeOwned + Default;
    type ViewState: serde::de::DeserializeOwned;
    type Tab: serde::de::DeserializeOwned;
    const MAJOR_VERSION: usize;
    const MINOR_VERSION: usize;
}

trait MigrateFrom<R: Reader>: Reader + Sized {
    fn migrate(r: ReadData<R>) -> ReadData<Self>;
}

struct ReadData<R: Reader> {
    meta: R::Metadata,
    views: HashMap<BlockIndex, R::ViewState>,
    world: R::WorldState,
    dock: egui_dock::DockState<R::Tab>,
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
        if raw.tag != TAG {
            return Err(ReadError::BadTag {
                expected: TAG.to_owned(),
                actual: raw.tag,
            });
        }
        let data = match raw.major {
            v1::MAJOR_VERSION => {
                let data = Self::deserialize_from_reader::<v1::Reader>(raw)?;
                v2::Reader::migrate(data)
            }
            v2::MAJOR_VERSION => {
                Self::deserialize_from_reader::<v2::Reader>(raw)?
            }
            i => {
                return Err(ReadError::BadMajorVersion {
                    actual_major: i,
                    expected_major: &[v1::MAJOR_VERSION, v2::MAJOR_VERSION],
                });
            }
        };

        Ok(Self {
            tag: TAG.to_owned(),
            major: MAJOR_VERSION,
            minor: MINOR_VERSION,
            meta: data.meta,
            views: data.views,
            world: data.world,
            dock: data.dock,
        })
    }

    fn deserialize_from_reader<R: Reader>(
        raw: RawAppState,
    ) -> Result<ReadData<R>, ReadError> {
        let too_new = || ReadError::TooNew {
            expected_major: R::MAJOR_VERSION,
            actual_major: raw.major,
            expected_minor: R::MINOR_VERSION,
            actual_minor: raw.minor,
        };
        let perhaps_too_new = raw.minor > R::MINOR_VERSION;
        let world: R::WorldState =
            serde_json::from_value(raw.world).map_err(|e| {
                if perhaps_too_new {
                    too_new()
                } else {
                    ReadError::from(e)
                }
            })?;
        let meta: R::Metadata = raw
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
        let mut views: HashMap<BlockIndex, R::ViewState> =
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
        Ok(ReadData {
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
