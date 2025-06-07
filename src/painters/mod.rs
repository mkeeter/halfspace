//! Native WGPU painters for rendering images

use eframe::egui_wgpu::wgpu;

mod bitmap;
mod clear;
mod heightmap;

pub use bitmap::WgpuBitmapPainter;
pub use heightmap::WgpuHeightmapPainter;

/// Universal basic GPU resources
///
/// This is constructed *once* and used for every GPU rendering task in the
/// GUI.
pub struct WgpuResources {
    bitmap_resources: bitmap::BitmapResources,
    heightmap_resources: heightmap::HeightmapResources,
    clear_resources: clear::ClearResources,
}

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
pub struct Uniforms {
    pub transform: [[f32; 4]; 4],
}

impl WgpuResources {
    pub fn reset(&mut self) {
        self.bitmap_resources.reset();
        self.heightmap_resources.reset();
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
        let clear_resources = clear::ClearResources::new(device, target_format);
        let heightmap_resources =
            heightmap::HeightmapResources::new(device, target_format);
        let bitmap_resources =
            bitmap::BitmapResources::new(device, target_format);

        WgpuResources {
            clear_resources,
            heightmap_resources,
            bitmap_resources,
        }
    }
}
