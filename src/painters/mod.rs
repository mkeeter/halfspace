//! Native WGPU painters for rendering images
//!
//! GPU rendering is integrated into the UI using [`egui_wgpu::CallbackTrait`].
//!
//! We use [`egui_wgpu::Callback::new_paint_callback`] to install a painter
//! (e.g. [`WgpuSdfPainter`]) when drawing the UI.  The painter does not have
//! any associated GPU resources; it just contains CPU-side data (e.g. image
//! buffers).
//!
//! During [`egui_wgpu::CallbackTrait::prepare`], the painter adds resources to
//! a global [`WgpuResources`] object.  The `WgpuResources` object is stored in
//! [`egui_wgpu::CallbackResources`], and contains both static data (e.g. render
//! pipelines) and maps of painter-specific data (e.g. textures).  During
//! `prepare`, the painter claims data to be used when drawing itself.
//!
//! Finally, in [`egui_wgpu::CallbackTrait::paint`], the painter grabs the data
//! that it previous installed in the global resources object and paints itself.
//!
//! After each frame, any unused data is deallocated.

use eframe::egui_wgpu::wgpu;

mod clear;
mod rgba;
mod sdf;

pub use rgba::{Uniforms as RgbaUniforms, WgpuRgbaPainter};
pub use sdf::{Uniforms as SdfUniforms, WgpuSdfPainter};

/// Universal basic GPU resources
///
/// This is constructed *once* and used for every GPU rendering task in the
/// GUI.  It is stored in [`CallbackResources`](egui_wgpu::CallbackResources),
/// which is available to all WGPU painters; additionally, a copy of the object
/// is stored in the main [`App`](crate::App) object.  All members are
/// reference-counted WGPU handles, so cloning should be cheap.
#[derive(Clone)]
pub struct WgpuResources {
    /// Render pipeline which clears the screen, drawing a grey checkerboard
    clear: clear::ClearResources,

    rgba: rgba::RgbaResources,
    sdf: sdf::SdfResources,
}

impl WgpuResources {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let clear = clear::ClearResources::new(device, target_format);

        let rgba = rgba::RgbaResources::new(device, target_format);
        let sdf = sdf::SdfResources::new(device, target_format);

        WgpuResources { clear, rgba, sdf }
    }

    pub fn rgba(&self) -> &rgba::RgbaResources {
        &self.rgba
    }

    pub fn sdf(&self) -> &sdf::SdfResources {
        &self.sdf
    }
}
