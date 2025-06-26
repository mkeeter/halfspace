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

mod bitfield;
mod clear;
mod heightmap;
mod sdf;
mod shaded;

pub use bitfield::WgpuBitfieldPainter;
pub use heightmap::WgpuHeightmapPainter;
pub use sdf::WgpuSdfPainter;
pub use shaded::WgpuShadedPainter;

/// Universal basic GPU resources
///
/// This is constructed *once* and used for every GPU rendering task in the
/// GUI.
pub struct WgpuResources {
    bitfield: bitfield::BitfieldResources,
    heightmap: heightmap::HeightmapResources,
    shaded: shaded::ShadedResources,
    clear: clear::ClearResources,
    sdf: sdf::SdfResources,
}

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
pub struct Uniforms {
    pub transform: [[f32; 4]; 4],
    pub color: [f32; 4],
}

impl WgpuResources {
    pub fn reset(&mut self) {
        self.bitfield.reset();
        self.heightmap.reset();
        self.shaded.reset();
        self.sdf.reset();
    }

    /// Installs an instance of `WgpuResources` into the callback resources
    pub fn install(wgpu_state: &eframe::egui_wgpu::RenderState) {
        let resources = Self::new(&wgpu_state.device, wgpu_state.target_format);
        wgpu_state
            .renderer
            .write()
            .callback_resources
            .insert(resources);
    }

    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let clear = clear::ClearResources::new(device, target_format);
        let heightmap =
            heightmap::HeightmapResources::new(device, target_format);
        let shaded = shaded::ShadedResources::new(device, target_format);
        let bitfield = bitfield::BitfieldResources::new(device, target_format);
        let sdf = sdf::SdfResources::new(device, target_format);

        WgpuResources {
            clear,
            heightmap,
            shaded,
            bitfield,
            sdf,
        }
    }
}
