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
    /// Associated type used to generate context notifications
    type Notify: Notify + Clone;

    /// Associated type for exporting files
    type ExportTarget: PlatformExport + std::fmt::Debug;

    fn new(ctx: &egui::Context, queue: MessageSender<Self::Notify>) -> Self;

    /// List all file names in local storage
    fn list_local_storage(&self) -> Vec<String>;

    /// Save a file to local storage
    fn save_to_local_storage(&self, path: &str, contents: &str);

    /// Reads a file from local storage
    fn read_from_local_storage(&self, path: &str) -> String;

    /// Downloads a chunk of data, returning the new modal
    fn download_file(
        &self,
        filename: &str,
        data: &[u8],
    ) -> Option<Modal<Self::ExportTarget>>;
    fn open(&self) -> Option<Modal<Self::ExportTarget>>;

    /// Returns `true` if `save` and `save_as` are valid
    fn can_save(&self) -> bool;

    /// Writes a file to a local path
    fn save(&self, state: &AppState, f: &Path) -> std::io::Result<()>;

    /// Opens a dialog to select a file name, then writes to that file
    fn save_as(&self, state: &AppState) -> std::io::Result<Option<PathBuf>>;

    /// Changes the window title
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
    ) -> Option<Self::ExportTarget>;
}

pub(crate) trait PlatformExport {
    fn save(&self, data: &[u8]) -> Result<(), std::io::Error>;
}

pub(crate) trait Notify: Send + Clone + 'static {
    type Err;
    fn wake(&self) -> Result<(), Self::Err>;
}
