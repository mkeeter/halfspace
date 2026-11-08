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
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;

mod undo;
mod v1;
mod v2;
mod v3;
pub use undo::Undo;
pub use v3::*;

#[derive(thiserror::Error, Debug)]
pub enum BinaryReadError {
    #[error("data stream is too short for tag and version")]
    TooShort,

    #[error("bad binary tag; expected {expected}, got {actual}")]
    BadTag { expected: u32, actual: u32 },

    #[error("unknown binary version {version}")]
    BadVersion { version: u32 },

    #[error("decompression read failed")]
    Decompression(#[source] std::io::Error),

    #[error("decompressed data was too large")]
    TooMuchData,

    #[error(transparent)]
    FromBytes(#[from] FromBytesError),
}

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
pub enum FromBytesError {
    #[error("data is not UTF-8")]
    NotUtf8(#[source] std::str::Utf8Error),

    #[error(transparent)]
    Deserialize(#[from] DeserializeError),
}

#[derive(thiserror::Error, Debug)]
pub enum DeserializeError {
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

    /// Builds an `AppState` from bytes representing JSON
    pub fn from_json_bytes(s: &[u8]) -> Result<Self, FromBytesError> {
        let d = std::str::from_utf8(s).map_err(FromBytesError::NotUtf8)?;
        Ok(Self::from_json(d)?)
    }

    /// Builds an `AppState` from a JSON string
    pub fn from_json(s: &str) -> Result<Self, DeserializeError> {
        let raw: RawAppState = serde_json::from_str(s)?;
        if raw.tag != TAG {
            return Err(DeserializeError::BadTag {
                expected: TAG.to_owned(),
                actual: raw.tag,
            });
        }
        let data = match raw.major {
            v1::MAJOR_VERSION => {
                let data = Self::deserialize_from_reader::<v1::Reader>(raw)?;
                let data = v2::Reader::migrate(data);
                v3::Reader::migrate(data)
            }
            v2::MAJOR_VERSION => {
                let data = Self::deserialize_from_reader::<v2::Reader>(raw)?;
                v3::Reader::migrate(data)
            }
            v3::MAJOR_VERSION => {
                Self::deserialize_from_reader::<v3::Reader>(raw)?
            }
            i => {
                return Err(DeserializeError::BadMajorVersion {
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
    ) -> Result<ReadData<R>, DeserializeError> {
        let too_new = || DeserializeError::TooNew {
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
                    DeserializeError::from(e)
                }
            })?;
        let meta: R::Metadata = raw
            .meta
            .map(|r| {
                serde_json::from_value(r).map_err(|e| {
                    if perhaps_too_new {
                        too_new()
                    } else {
                        DeserializeError::from(e)
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
                    DeserializeError::from(e)
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

    pub fn to_binary(&self) -> Vec<u8> {
        let mut compressed = vec![];
        compressed.extend(BIN_TAG.to_le_bytes());
        compressed.extend(BIN_VERSION_JSON_BROTLI.to_le_bytes());
        let json_buffer = serde_json::ser::to_vec(&self).unwrap();

        use brotli::enc::{
            BrotliEncoderParams, backward_references::BrotliEncoderMode,
        };
        let params = BrotliEncoderParams {
            quality: 11,
            mode: BrotliEncoderMode::BROTLI_MODE_TEXT,
            size_hint: json_buffer.len(),
            // Smallest window that covers the input, clamped to the valid range
            lgwin: (usize::BITS
                - json_buffer.len().saturating_sub(1).leading_zeros())
            .clamp(10, 24) as i32,
            ..Default::default()
        };

        brotli::BrotliCompress(&mut &json_buffer[..], &mut compressed, &params)
            .unwrap();
        debug!("compressed to {} bytes", compressed.len());
        compressed
    }

    pub fn from_binary(data: &[u8]) -> Result<Self, BinaryReadError> {
        if data.len() < 8 {
            return Err(BinaryReadError::TooShort);
        }
        let tag = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        if tag != BIN_TAG {
            return Err(BinaryReadError::BadTag {
                expected: BIN_TAG,
                actual: tag,
            });
        }
        if version != BIN_VERSION_JSON_BROTLI {
            return Err(BinaryReadError::BadVersion { version });
        }

        let mut decompressed = vec![];
        let d = brotli::Decompressor::new(&data[8..], 4096);

        // Limit to 32 MiB of file size to protect against decompression bombs
        // (that's very large, given our typical file sizes!)
        const MAX_DECOMPRESSED_SIZE: u64 = 32 * 1024 * 1024;
        d.take(MAX_DECOMPRESSED_SIZE + 1)
            .read_to_end(&mut decompressed)
            .map_err(BinaryReadError::Decompression)?;
        if decompressed.len() as u64 > MAX_DECOMPRESSED_SIZE {
            return Err(BinaryReadError::TooMuchData);
        }
        let out = Self::from_json_bytes(&decompressed)?;
        Ok(out)
    }
}

const BIN_TAG: u32 = 0x68616c66; // 'half'

/// Flag indicating JSON compressed with Brotli
const BIN_VERSION_JSON_BROTLI: u32 = 1;

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
