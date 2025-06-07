//! Native WGPU painters for rendering images

use eframe::egui_wgpu::wgpu;

mod bitfield;
mod clear;
mod heightmap;
mod sdf;

pub use bitfield::WgpuBitfieldPainter;
pub use heightmap::WgpuHeightmapPainter;
pub use sdf::WgpuSdfPainter;

/// Universal basic GPU resources
///
/// This is constructed *once* and used for every GPU rendering task in the
/// GUI.
pub struct WgpuResources {
    bitfield: bitfield::BitfieldResources,
    heightmap: heightmap::HeightmapResources,
    clear: clear::ClearResources,
    sdf: sdf::SdfResources,
}

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
pub struct Uniforms {
    pub transform: [[f32; 4]; 4],
}

impl WgpuResources {
    pub fn reset(&mut self) {
        self.bitfield.reset();
        self.heightmap.reset();
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
        let bitfield = bitfield::BitfieldResources::new(device, target_format);
        let sdf = sdf::SdfResources::new(device, target_format);

        WgpuResources {
            clear,
            heightmap,
            bitfield,
            sdf,
        }
    }
}
