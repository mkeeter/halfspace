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
mod debug;
mod heightmap;
mod sdf;
mod shaded;

pub use bitfield::WgpuBitfieldPainter;
pub use debug::WgpuDebugPainter;
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
    debug: debug::DebugResources,
    clear: clear::ClearResources,
    sdf: sdf::SdfResources,
}

impl WgpuResources {
    pub fn reset(&mut self) {
        self.bitfield.reset();
        self.heightmap.reset();
        self.shaded.reset();
        self.sdf.reset();
        self.debug.reset();
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
        let debug = debug::DebugResources::new(device, target_format);

        WgpuResources {
            clear,
            heightmap,
            shaded,
            bitfield,
            sdf,
            debug,
        }
    }
}

/// Computes a transform matrix to put a 2D image in the right place on a canvas
fn transform2(
    image_view: fidget::render::View2,
    image_size: fidget::render::ImageSize,
    canvas_view: fidget::render::View2,
    canvas_size: fidget::render::ImageSize,
) -> nalgebra::Matrix4<f32> {
    // don't blame me, I just twiddled the matrices until things
    // looked right
    let aspect_ratio = |size: fidget::render::ImageSize| {
        let width = size.width() as f32;
        let height = size.height() as f32;
        if width > height {
            nalgebra::Scale2::new(height / width, 1.0)
        } else {
            nalgebra::Scale2::new(1.0, width / height)
        }
    };
    let prev_aspect_ratio = aspect_ratio(image_size);
    let curr_aspect_ratio = aspect_ratio(canvas_size);
    let m = prev_aspect_ratio.to_homogeneous().try_inverse().unwrap()
        * curr_aspect_ratio.to_homogeneous()
        * canvas_view.world_to_model().try_inverse().unwrap()
        * image_view.world_to_model();

    #[rustfmt::skip]
    let transform = nalgebra::Matrix4::new(
        m[(0, 0)], m[(0, 1)], 0.0, m[(0, 2)] * curr_aspect_ratio.x,
        m[(1, 0)], m[(1, 1)], 0.0, m[(1, 2)] * curr_aspect_ratio.y,
        0.0,         0.0,         1.0, 0.0,
        0.0,         0.0,         0.0, 1.0,
    );
    transform
}

/// Computes a transform matrix to put a 3D image in the right place on a canvas
fn transform3(
    image_view: fidget::render::View3,
    image_size: fidget::render::VoxelSize,
    canvas_view: fidget::render::View3,
    canvas_size: fidget::render::ImageSize,
) -> nalgebra::Matrix4<f32> {
    // don't blame me, I just twiddled the matrices until things
    // looked right
    let aspect_ratio = |width: u32, height: u32| {
        let width = width as f32;
        let height = height as f32;
        if width > height {
            nalgebra::Scale3::new(height / width, 1.0, 1.0)
        } else {
            nalgebra::Scale3::new(1.0, width / height, 1.0)
        }
    };
    let prev_aspect_ratio =
        aspect_ratio(image_size.width(), image_size.height());
    let curr_aspect_ratio =
        aspect_ratio(canvas_size.width(), canvas_size.height());
    let m = prev_aspect_ratio.to_homogeneous().try_inverse().unwrap()
        * curr_aspect_ratio.to_homogeneous()
        * canvas_view.world_to_model().try_inverse().unwrap()
        * image_view.world_to_model();

    #[rustfmt::skip]
    let transform = nalgebra::Matrix4::new(
        m[(0, 0)], m[(0, 1)], m[(0, 2)], m[(0, 3)] * curr_aspect_ratio.x,
        m[(1, 0)], m[(1, 1)], m[(1, 2)], m[(1, 3)] * curr_aspect_ratio.y,
        m[(2, 0)], m[(2, 1)], m[(2, 2)], m[(2, 3)],
        0.0,         0.0,         0.0, 1.0,
    );
    transform
}
