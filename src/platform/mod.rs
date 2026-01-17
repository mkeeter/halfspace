use crate::{AppState, MessageSender, Modal};
use std::path::{Path, PathBuf};

#[cfg_attr(target_arch = "wasm32", path = "web.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "native.rs")]
mod inner;

pub use inner::run;

pub(crate) trait Platform
where
    Self: Sized,
{
    type Data: PlatformData<Self>;
    type ExportTarget: PlatformExport + std::fmt::Debug;
    type Notify: Notify + Clone;
}

pub(crate) trait PlatformExport {
    fn save(&self, data: &[u8]) -> Result<(), std::io::Error>;
}

pub(crate) trait PlatformData<P: Platform> {
    fn new(ctx: &egui::Context, queue: MessageSender<P::Notify>) -> Self;
    fn list_local_storage(&self) -> Vec<String>;
    fn save_to_local_storage(&self, path: &str, contents: &str);
    fn read_from_local_storage(&self, path: &str) -> String;
    fn download_file(
        &self,
        filename: &str,
        _data: &[u8],
    ) -> Option<Modal<P::ExportTarget>>;
    fn open(&self) -> Option<Modal<P::ExportTarget>>;

    /// Returns `true` if `save` and `save_as` are valid
    fn can_save(&self) -> bool;
    fn save(&self, state: &AppState, f: &Path) -> std::io::Result<()>;
    fn save_as(&self, state: &AppState) -> std::io::Result<Option<PathBuf>>;

    fn update_title(&self, title: &str);

    /// Returns a target to be used when exporting files
    ///
    /// The `name` argument is a hint provided in the file's metadata; other
    /// arguments determine the parameters for a file dialog
    fn export_name(
        &self,
        name: Option<&str>,
        dialog_name: &str,
        extension: &str,
    ) -> Option<P::ExportTarget>;
}

pub(crate) trait Notify: Send + Clone + 'static {
    type Err;
    fn wake(&self) -> Result<(), Self::Err>;
}
