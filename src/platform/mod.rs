#[cfg_attr(target_arch = "wasm32", path = "web.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "native.rs")]
mod inner;

pub use inner::run;
pub(crate) use inner::{Data, ExportTarget};
