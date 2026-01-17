use crate::{AppState, MessageSender, Modal};
use std::path::{Path, PathBuf};

#[cfg_attr(target_arch = "wasm32", path = "web.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "native.rs")]
mod inner;

pub(crate) use inner::ExportTarget;
pub use inner::run;

pub(crate) trait Platform
where
    Self: Sized,
{
    type Data: PlatformData<Self>;
    type ExportTarget: std::fmt::Debug;
    type Notify: Notify + Clone;
}

pub(crate) trait PlatformData<P: Platform> {
    fn new(queue: MessageSender<P::Notify>) -> Self;
    fn list_local_storage(&self) -> Vec<String>;
    fn save_to_local_storage(&self, path: &str, contents: &str);
    fn read_from_local_storage(&self, path: &str) -> String;
    fn download_file(&self, filename: &str, _data: &[u8]) -> Option<Modal>;
    fn open(&self) -> Option<Modal>;
    fn save(&self, state: &AppState, f: &Path) -> std::io::Result<()>;
    fn save_as(&self, state: &AppState) -> std::io::Result<Option<PathBuf>>;
}

pub(crate) trait Notify: Send + Clone + 'static {
    type Err;
    fn wake(&self) -> Result<(), Self::Err>;
}
