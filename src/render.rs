//! Image rendering
//!
//! # Big Theory Statement
//! Each block in the GUI may have 0 or 1 views (represented by a
//! [`ViewData`](crate::view::ViewData)).  The [`App`](crate::App) stores a map
//! from `BlockIndex` to `ViewData`.
//!
//! When updating the UI, we construct a [`WorldView`](crate::gui::WorldView),
//! which implements the [`egui_dock::TabViewer`] trait.  When a view is drawn,
//! we call [`WorldView::view_ui`](crate::gui::WorldView::view_ui), which grabs
//! the `ViewData` for that block.  This in turn calls
//! [`ViewData::image`](crate::view::ViewData::image) to get a [`ViewImage`] to
//! draw.
//!
//! From here, our dive goes in two directions.
//!
//! ## Rendering images
//! [`ViewData::image`](crate::view::ViewData::image) checks to see whether our
//! current settings match those of an in-progress render.  If not, then it
//! cancels the in-progress render and starts a new render, spawning it into the
//! render worker pool.  If available, it returns a cached image, which is a
//! [`ViewImage`].
//!
//! A render task is represented by a [`RenderTask`] object, which performs the
//! render then sends a generation-tagged result into a [`MessageGenSender`].
//! Note that there are **two** generations: a global generation associated with
//! the `App`, and a local generation associated with the `ViewData`.  The
//! global generation invalidates messages associated with a previous file; the
//! local generation invalidates render results which arrive out of order (only
//! the newest render task has the correct local generation number).
//!
//! Eventually, the [`RenderTask`] finishes.  It sends [`Message::RenderView`]
//! into the global event queue; the main loop receives it and dispatches to the
//! appropriate [`ViewData::update`](crate::view::ViewData::update).
//!
//! In [`ViewData::update`](crate::view::ViewData::update), the new image data
//! is stored and we adjust the `start_level` based on render time; this is used
//! in subsequent renders to maintain a high frame rate.
//!
//! At the end of this process, we have a [`ViewImage`], which contains pixels
//! in RAM for a particular image type and render settings (angle, image size,
//! etc).  We store this image (and the settings used to generate it) into the
//! `ViewData`, for use in the next check.
//!
//! ## Drawing images to the screen
//! TODO write this
use crate::{
    BlockIndex, Message, MessageGenSender, MessageSender, RenderViewReply,
    export::ExportError,
    platform::Notify,
    view::{ViewCanvas, ViewMode2, ViewMode3},
    world::{Drawable, Scene},
};

use by_address::ByAddress;
use egui_wgpu::wgpu;
use fidget::{
    raster::pixel::RawDistancePixel,
    render::{ImageSize, VoxelSize},
    wgpu::Context as _, // trait import
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use web_time::Instant;
use zerocopy::{FromBytes, IntoBytes};

/// Rendered image to be sent back to the main thread
pub enum RenderImage {
    Pixel {
        image: PixelImage,
        settings: ImageRenderSettings,
    },
    Voxel {
        image: RgbaImage,
        settings: VoxelRenderSettings,
    },
}

impl RenderImage {
    pub fn level(&self) -> usize {
        match self {
            RenderImage::Pixel { settings, .. } => settings.level,
            RenderImage::Voxel { settings, .. } => settings.level,
        }
    }

    pub fn settings(&self) -> RenderSettings {
        match self {
            RenderImage::Pixel { settings, .. } => {
                RenderSettings::Image(settings.clone())
            }
            RenderImage::Voxel { settings, .. } => {
                RenderSettings::Voxel(settings.clone())
            }
        }
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    zerocopy::IntoBytes,
    zerocopy::FromBytes,
    zerocopy::Immutable,
)]
pub struct Float16(pub u16);

#[derive(Clone)]
pub struct PixelImage {
    pub distance: Vec<Float16>,
    pub color: Option<Vec<[u8; 4]>>,
    pub size: ImageSize,
}

#[derive(Clone)]
pub struct RgbaImage {
    pub color: Vec<u32>,
    pub size: VoxelSize,
}

/// State representing an in-progress render
///
/// This lives in the main thread; the work itself lives in [`RenderTask`] in
/// the worker pool.
pub struct RenderTaskHandle {
    settings: RenderSettings,
    cancel: fidget::render::CancelToken,
}

impl Drop for RenderTaskHandle {
    fn drop(&mut self) {
        self.cancel.cancel()
    }
}

/// Render worker pool, backed by dedicated threads or web workers
pub(crate) struct RenderWorkerPool<N: Notify> {
    tx: flume::Sender<RenderTask<N>>,
}

impl<N: Notify> RenderWorkerPool<N> {
    pub(crate) fn new(tx: flume::Sender<RenderTask<N>>) -> Self {
        Self { tx }
    }

    /// Begins a new image rendering task in the worker pool
    pub(crate) fn spawn(
        &self,
        block: BlockIndex,
        generation: u64,
        settings: RenderSettings,
        tx: MessageGenSender<N>,
    ) -> RenderTaskHandle {
        let cancel = fidget::render::CancelToken::new();
        let start_time = Instant::now();
        let task = RenderTask::Render {
            kind: TaskKind::Display {
                block,
                generation,
                start_time,
                reply: tx,
            },
            settings: settings.clone(),
            cancel: cancel.clone(),
        };
        self.tx.send(task).expect("render thread stopped");
        RenderTaskHandle { settings, cancel }
    }

    pub(crate) fn end_frame(&self, active: HashSet<BlockIndex>) {
        self.tx
            .send(RenderTask::EndFrame { active })
            .expect("render thread stopped");
    }

    /// Begins a new export image rendering task in the worker pool
    pub(crate) fn export(
        &self,
        settings: RenderSettings,
        tx: MessageSender<N>,
    ) -> fidget::render::CancelToken {
        let cancel = fidget::render::CancelToken::new();
        let task = RenderTask::Render {
            kind: TaskKind::Export { reply: tx },
            settings,
            cancel: cancel.clone(),
        };
        self.tx.send(task).expect("render thread stopped");
        cancel
    }
}

impl RenderTaskHandle {
    /// Checks whether a task should be cancelled, based on new settings
    ///
    /// The `next.level` field should be a max-level value, i.e. a value at
    /// which we expect interactive rendering.  We don't cancel max-level render
    /// tasks, because we want to preserve interactivity.
    pub fn should_cancel(&self, next: &RenderSettings) -> bool {
        !self.settings.matches(next) && self.settings.level() != next.level()
    }
}

/// Object representing a render task
pub enum RenderTask<N: Notify> {
    Render {
        settings: RenderSettings,
        cancel: fidget::render::CancelToken,
        kind: TaskKind<N>,
    },
    EndFrame {
        active: HashSet<BlockIndex>,
    },
}

pub enum TaskKind<N: Notify> {
    Display {
        block: BlockIndex,
        generation: u64,
        reply: MessageGenSender<N>,
        start_time: Instant,
    },
    Export {
        reply: MessageSender<N>,
    },
}

/// Settings for rendering an image
#[derive(Clone)]
pub enum RenderSettings {
    Image(ImageRenderSettings),
    Voxel(VoxelRenderSettings),
}

impl RenderSettings {
    fn scene(&self) -> &Scene {
        match self {
            RenderSettings::Voxel(settings) => &settings.scene,
            RenderSettings::Image(settings) => &settings.scene,
        }
    }
}

#[derive(Clone)]
pub struct ImageRenderSettings {
    pub scene: Scene,
    pub mode: ViewMode2,
    pub view: fidget::gui::View2,
    /// Size of the full-resolution image
    pub base_size: fidget::render::ImageSize,
    /// Downsampling level (0 is full-resolution, N is 2^N smaller)
    pub level: usize,
}

impl ImageRenderSettings {
    /// Returns the size at which the image should be rendered
    pub fn render_size(&self) -> fidget::render::ImageSize {
        let scale = 1 << self.level;
        fidget::render::ImageSize::new(
            (self.base_size.width() / scale).max(1),
            (self.base_size.height() / scale).max(1),
        )
    }

    /// Returns `true` if the render settings are identical, other than `level`
    pub fn matches(&self, other: &ImageRenderSettings) -> bool {
        // Manual destructuring to fail loudly if things change
        let Self {
            scene,
            mode,
            view,
            base_size,
            level: _,
        } = self;
        scene == &other.scene
            && mode == &other.mode
            && view == &other.view
            && base_size == &other.base_size
    }
}

#[derive(Clone)]
pub struct VoxelRenderSettings {
    pub scene: Scene,
    pub mode: ViewMode3,
    pub perspective: bool,
    pub view: fidget::gui::View3,
    /// Size of the full-resolution image
    pub base_size: fidget::render::VoxelSize,
    /// Downsampling level (0 is full-resolution, N is 2^N smaller)
    pub level: usize,
}

impl VoxelRenderSettings {
    /// Returns the size at which the image should be rendered
    pub fn render_size(&self) -> fidget::render::VoxelSize {
        // If this is our final rendering level, then do oversampling in
        // the Z direction for better rendering of edges.
        let scale = 1 << self.level;
        let bonus_z = if self.level == 0 { 2 } else { 1 };
        fidget::render::VoxelSize::new(
            (self.base_size.width() / scale).max(1),
            (self.base_size.height() / scale).max(1),
            (self.base_size.depth() / scale).max(1) * bonus_z,
        )
    }

    /// Returns a scale which compensates for bonus Z voxels at level == 0
    pub fn scale(&self) -> nalgebra::Scale3<f32> {
        let bonus_z = if self.level == 0 { 2 } else { 1 };
        let z_scale = 2.0 / bonus_z as f32;
        nalgebra::Scale3::new(1.0, 1.0, z_scale)
    }

    /// Returns `true` if the render settings are identical, other than `level`
    pub fn matches(&self, other: &VoxelRenderSettings) -> bool {
        // Manual destructuring to fail loudly if things change
        let Self {
            scene,
            mode,
            perspective,
            view,
            base_size,
            level: _,
        } = self;
        scene == &other.scene
            && mode == &other.mode
            && perspective == &other.perspective
            && view == &other.view
            && base_size == &other.base_size
    }
}

impl RenderSettings {
    pub fn from_canvas(
        canvas: &ViewCanvas,
        scene: Scene,
        level: usize,
    ) -> Self {
        match canvas {
            ViewCanvas::Canvas2 { canvas, mode } => {
                RenderSettings::Image(ImageRenderSettings {
                    scene,
                    view: canvas.view(),
                    base_size: canvas.image_size(),
                    mode: *mode,
                    level,
                })
            }
            ViewCanvas::Canvas3 {
                canvas,
                mode,
                perspective,
            } => {
                let size = canvas.image_size();
                RenderSettings::Voxel(VoxelRenderSettings {
                    scene,
                    view: canvas.view(),
                    perspective: *perspective,
                    base_size: fidget::render::VoxelSize::new(
                        size.width(),
                        size.height(),
                        // XXX select depth?
                        size.width().max(size.height()),
                    ),
                    mode: *mode,
                    level,
                })
            }
        }
    }

    pub fn level(&self) -> usize {
        match self {
            RenderSettings::Image(s) => s.level,
            RenderSettings::Voxel(s) => s.level,
        }
    }

    pub fn with_level(&self, next_level: usize) -> Self {
        let mut out = self.clone();
        match &mut out {
            RenderSettings::Image(ImageRenderSettings { level, .. })
            | RenderSettings::Voxel(VoxelRenderSettings { level, .. }) => {
                *level = next_level;
            }
        }
        out
    }

    /// Returns `true` if the render settings are identical, other than `level`
    pub fn matches(&self, other: &RenderSettings) -> bool {
        match (self, other) {
            (RenderSettings::Voxel(va), RenderSettings::Voxel(vb)) => {
                va.matches(vb)
            }
            (RenderSettings::Image(ia), RenderSettings::Image(ib)) => {
                ia.matches(ib)
            }
            _ => false,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Render worker, to be run in a thread (native) or Web Worker (web)
pub(crate) async fn render_worker<N: Notify>(
    mut gpu: GpuWorker,
    rx: flume::Receiver<RenderTask<N>>,
    waker: flume::Sender<bool>,
) {
    while let Ok(task) = rx.recv_async().await {
        let _ = waker.try_send(true);
        match task {
            RenderTask::Render {
                settings,
                cancel,
                kind,
            } => match kind {
                TaskKind::Display {
                    block,
                    generation,
                    start_time,
                    reply,
                } => {
                    if cancel.is_cancelled() {
                        continue;
                    }
                    // Grab the shape from the global shape cache; we'll put it
                    // back at the end of rendering.
                    let scene = settings.scene().clone();
                    let mut shape = gpu
                        .shapes
                        .remove(&(block, ByAddress(scene.shapes.clone())))
                        .unwrap_or_else(|| RenderShapes::new(&scene, &gpu.gpu));

                    // Dispatch to either voxel or pixel rendering
                    let image = match settings {
                        RenderSettings::Voxel(settings) => {
                            let image_size = settings.render_size();
                            let mut buf =
                                gpu.get_voxel_buffers(block, image_size);
                            let image = gpu
                                .render_voxel(&settings, &mut buf, &mut shape)
                                .await;
                            gpu.store_voxel_buffers(block, image_size, buf);
                            RenderImage::Voxel { image, settings }
                        }
                        RenderSettings::Image(settings) => {
                            let image_size = settings.render_size();
                            let mut buf =
                                gpu.get_pixel_buffers(block, image_size);
                            let image = gpu
                                .render_pixel_f16(
                                    &settings, &mut buf, &mut shape,
                                )
                                .await;
                            gpu.store_pixel_buffers(block, image_size, buf);
                            RenderImage::Pixel { image, settings }
                        }
                    };
                    // Restore the shape in the cache
                    gpu.shapes.insert((block, ByAddress(scene.shapes)), shape);
                    reply.send(Message::RenderView(RenderViewReply {
                        block,
                        generation,
                        start_time,
                        image,
                    }))
                }
                TaskKind::Export { reply } => {
                    // Initial check for cancellation, in case there's a long
                    // queue of things to render and we just now got to this
                    // one.
                    if cancel.is_cancelled() {
                        reply.send(Message::ExportComplete(Err(
                            ExportError::Cancelled,
                        )));
                        continue;
                    }

                    // We can't pass cancellation through to the GPU,
                    // unfortunately, so we'll kick off a rendering then check
                    // cancellation again after we're done.
                    enum ExportImage {
                        Rgba(RgbaImage),
                        Distance {
                            distance: Vec<RawDistancePixel>,
                            color: Option<Vec<[u8; 4]>>,
                        },
                    }
                    // We will build bespoke shape and voxel/pixel buffers for
                    // export, instead of trying to borrow one from a scene.
                    // Exports should be a rare (human-time-scale) event, so
                    // we're not optimizing for peak efficiency.
                    let scene = settings.scene();
                    let mut shapes = RenderShapes::new(scene, &gpu.gpu);
                    let (data, image_size) = match &settings {
                        RenderSettings::Voxel(vs) => {
                            let mut buf =
                                gpu.build_voxel_buffers(vs.render_size());
                            let image = gpu
                                .render_voxel(vs, &mut buf, &mut shapes)
                                .await;
                            let size = image.size.into();
                            (ExportImage::Rgba(image), size)
                        }
                        RenderSettings::Image(rs) => {
                            // Build temporary pixel buffers, which aren't stored in
                            // the GPU object
                            let mut buf =
                                gpu.build_pixel_buffers(rs.render_size());
                            let (distance, color) = gpu
                                .render_pixel_f32(rs, &mut buf, &mut shapes)
                                .await;
                            let size = rs.render_size();
                            (ExportImage::Distance { distance, color }, size)
                        }
                    };

                    // Re-check cancellation.  This is a *little* silly, because
                    // I expect image rendering to happen in ~milliseconds, but
                    // maybe the user wants a gigantic export and will have time
                    // to hit Cancel?
                    if cancel.is_cancelled() {
                        reply.send(Message::ExportComplete(Err(
                            ExportError::Cancelled,
                        )));
                        continue;
                    }

                    let out = match &data {
                        ExportImage::Distance { distance, color } => {
                            if let Some(c) = color {
                                std::borrow::Cow::Borrowed(c.as_bytes())
                            } else {
                                distance
                                    .iter()
                                    .flat_map(|p| {
                                        if p.inside() {
                                            [0xFF; 4]
                                        } else {
                                            [0x00; 4]
                                        }
                                    })
                                    .collect()
                            }
                        }
                        ExportImage::Rgba(im) => {
                            std::borrow::Cow::Borrowed(im.color.as_bytes())
                        }
                    };

                    let mut bytes = vec![];
                    match image::write_buffer_with_format(
                        &mut std::io::Cursor::new(&mut bytes),
                        &out,
                        image_size.width(),
                        image_size.height(),
                        image::ColorType::Rgba8,
                        image::ImageFormat::Png,
                    ) {
                        Ok(()) => {
                            reply.send(Message::ExportComplete(Ok(bytes)))
                        }
                        Err(e) => {
                            reply.send(Message::ExportComplete(Err(e.into())))
                        }
                    }
                }
            },
            RenderTask::EndFrame { active } => gpu.end_frame(active),
        }
        let _ = waker.try_send(false);
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct VoxelContext {
    render: fidget::wgpu::voxel::render::Context,
    merge: fidget::wgpu::voxel::merge::Context,
    shade: fidget::wgpu::voxel::shade::Context,
    ssao: fidget::wgpu::voxel::ssao::Context,
    color: fidget::wgpu::voxel::color::Context,
    heightmap: fidget::wgpu::voxel::heightmap::Context,
}

pub struct VoxelWorkspace {
    render: fidget::wgpu::voxel::render::Workspace,
    merge: fidget::wgpu::voxel::merge::Workspace,
    shade: fidget::wgpu::voxel::shade::Workspace,
    ssao: fidget::wgpu::voxel::ssao::Workspace,
    color: fidget::wgpu::voxel::color::Workspace,
    heightmap: fidget::wgpu::voxel::heightmap::Workspace,
}

pub struct VoxelBuffers {
    render: fidget::wgpu::voxel::render::Buffers,
    merge: fidget::wgpu::voxel::merge::Buffers,
    shade: fidget::wgpu::voxel::shade::Buffers,
    ssao: fidget::wgpu::voxel::ssao::Buffers,
    read: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::voxel::shade::ShadedImageTag,
    >,
}

pub struct PixelContext {
    render: fidget::wgpu::pixel::render::Context,
    merge: fidget::wgpu::pixel::merge::Context,
    color: fidget::wgpu::pixel::color::Context,
}

pub struct PixelWorkspace {
    render: fidget::wgpu::pixel::render::Workspace,
    merge: fidget::wgpu::pixel::merge::Workspace,
    color: fidget::wgpu::pixel::color::Workspace,
}

pub struct PixelBuffers {
    render: fidget::wgpu::pixel::render::Buffers,
    merge: fidget::wgpu::pixel::merge::Buffers,

    // We use a raw buffer for the `f16` because we have to align it manually
    distance_buffer_f16: wgpu::Buffer,
    read_distance_buffer_f16: wgpu::Buffer,

    read_distance_buffer_f32: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::pixel::merge::PixelDistanceBufferTag,
    >,
    read_color_buffer: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::pixel::merge::PixelColorBufferTag,
    >,
}

struct RenderShapes {
    shapes: Vec<fidget::wgpu::RenderShape>,
    color: Option<fidget::wgpu::color::RenderColor>,
}

impl RenderShapes {
    fn new(scene: &Scene, gpu: &fidget::wgpu::Gpu) -> Self {
        let shapes = scene
            .shapes
            .iter()
            .map(|s| {
                let rs = s.tree.clone().into();
                gpu.build_render_shape(&rs)
                    .expect("failed to get render shape")
            })
            .collect::<Vec<_>>();
        let color = if scene.shapes.iter().any(|c| c.color.is_some()) {
            let colors = scene
                .shapes
                .iter()
                .map(|t| {
                    t.color
                        .as_ref()
                        .map(fidget::wgpu::color::ShapeColor::from)
                        .unwrap_or_else(|| {
                            let c = fidget::context::Tree::constant(1.0);
                            fidget::wgpu::color::ShapeColor::Rgb {
                                r: c.clone().into(),
                                g: c.clone().into(),
                                b: c.clone().into(),
                            }
                        })
                })
                .collect::<Vec<_>>();
            Some(gpu.build_render_color(&colors).unwrap())
        } else {
            None
        };
        RenderShapes { shapes, color }
    }
}

pub(crate) struct GpuWorker {
    pub gpu: fidget::wgpu::Gpu,

    voxel_ctx: VoxelContext,
    voxel_workspace: VoxelWorkspace,
    voxel_buffers: HashMap<(BlockIndex, VoxelSize), VoxelBuffers>,

    pixel_ctx: PixelContext,
    pixel_workspace: PixelWorkspace,
    pixel_buffers: HashMap<(BlockIndex, ImageSize), PixelBuffers>,

    pixel_downfloat_pipeline: wgpu::ComputePipeline,
    pixel_downfloat_bind_group_layout: wgpu::BindGroupLayout,

    shapes: HashMap<ShapeKey, RenderShapes>,
}

type ShapeKey = (BlockIndex, ByAddress<Arc<[Drawable]>>);

impl GpuWorker {
    pub(crate) async fn new() -> Self {
        let gpu = fidget::wgpu::Gpu::init().await.unwrap();
        let voxel_ctx = VoxelContext {
            render: fidget::wgpu::voxel::render::Context::new(&gpu),
            merge: fidget::wgpu::voxel::merge::Context::new(&gpu),
            ssao: fidget::wgpu::voxel::ssao::Context::new(&gpu),
            color: fidget::wgpu::voxel::color::Context::new(&gpu),
            shade: fidget::wgpu::voxel::shade::Context::new(&gpu),
            heightmap: fidget::wgpu::voxel::heightmap::Context::new(&gpu),
        };
        let voxel_workspace = VoxelWorkspace {
            render: voxel_ctx.render.workspace(),
            merge: voxel_ctx.merge.workspace(),
            ssao: voxel_ctx.ssao.workspace(),
            color: voxel_ctx.color.workspace(),
            shade: voxel_ctx.shade.workspace(),
            heightmap: voxel_ctx.heightmap.workspace(),
        };

        let pixel_ctx = PixelContext {
            render: fidget::wgpu::pixel::render::Context::new(&gpu),
            merge: fidget::wgpu::pixel::merge::Context::new(&gpu),
            color: fidget::wgpu::pixel::color::Context::new(&gpu),
        };
        let pixel_workspace = PixelWorkspace {
            render: pixel_ctx.render.workspace(),
            merge: pixel_ctx.merge.workspace(),
            color: pixel_ctx.color.workspace(),
        };

        // We're going to build a simple pipeline for f32 -> f16 conversion
        let pixel_downfloat_bind_group_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("common bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage {
                                read_only: true,
                            },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage {
                                read_only: false,
                            },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let pipeline_layout = gpu.device.create_pipeline_layout(
            &wgpu::PipelineLayoutDescriptor {
                label: Some("float conversion pipeline"),
                bind_group_layouts: &[Some(&pixel_downfloat_bind_group_layout)],
                immediate_size: 0u32,
            },
        );
        let shader_module =
            gpu.device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("float shader module"),
                    source: wgpu::ShaderSource::Wgsl(
                        include_str!(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/shaders/downfloat.wgsl"
                        ))
                        .into(),
                    ),
                });
        let pixel_downfloat_pipeline = gpu.device.create_compute_pipeline(
            &wgpu::ComputePipelineDescriptor {
                label: Some("float compute pipeline"),
                layout: Some(&pipeline_layout),
                module: &shader_module,
                entry_point: Some("float_main"),
                compilation_options: Default::default(),
                cache: None,
            },
        );

        Self {
            gpu,
            voxel_ctx,
            voxel_workspace,
            voxel_buffers: HashMap::new(),

            pixel_ctx,
            pixel_workspace,
            pixel_buffers: HashMap::new(),

            pixel_downfloat_pipeline,
            pixel_downfloat_bind_group_layout,

            shapes: HashMap::new(),
        }
    }

    async fn render_voxel(
        &mut self,
        vs: &VoxelRenderSettings,
        voxel_buffers: &mut VoxelBuffers,
        shapes: &mut RenderShapes,
    ) -> RgbaImage {
        let VoxelRenderSettings {
            mode,
            view,
            perspective,
            ..
        } = vs;
        let image_size = vs.render_size();
        let scale = vs.scale();
        let mut world_to_model = view.world_to_model() * scale.to_homogeneous();
        if *perspective {
            *world_to_model.get_mut((3, 2)).unwrap() = 0.15 * scale.z;
        }
        let render_cfg = fidget::wgpu::voxel::render::Settings {
            world_to_model,
            vars: &Default::default(),
        };

        // Render and accumulate every shape into merge buffers
        voxel_buffers.merge.reset();
        let merge_settings = fidget::wgpu::voxel::merge::Settings {
            denoise: true,
            z_scale: scale.z,
        };

        for s in shapes.shapes.iter() {
            self.voxel_ctx
                .render
                .submit(
                    s,
                    render_cfg,
                    &mut self.voxel_workspace.render,
                    &mut voxel_buffers.render,
                )
                .expect("failed to submit voxel render");
            self.voxel_ctx
                .merge
                .submit(
                    &voxel_buffers.render,
                    merge_settings,
                    &mut self.voxel_workspace.merge,
                    &mut voxel_buffers.merge,
                )
                .expect("failed to submit voxel merge");
        }
        if let Some(colors) = shapes.color.as_ref() {
            self.voxel_ctx
                .color
                .submit(
                    fidget::wgpu::voxel::color::Args {
                        image: &voxel_buffers.merge,
                        color: colors,
                    },
                    fidget::wgpu::voxel::color::Settings {
                        world_to_model,
                        vars: &Default::default(),
                    },
                    &mut self.voxel_workspace.color,
                    &mut voxel_buffers.shade,
                )
                .expect("failed to submit color rendering");
        }

        match mode {
            ViewMode3::Heightmap => self
                .voxel_ctx
                .heightmap
                .submit(
                    &voxel_buffers.merge,
                    (),
                    &mut self.voxel_workspace.heightmap,
                    &mut voxel_buffers.shade,
                )
                .expect("failed to submit shaded rendering"),
            ViewMode3::Shaded => {
                self.voxel_ctx
                    .ssao
                    .submit(
                        &voxel_buffers.merge,
                        (),
                        &mut self.voxel_workspace.ssao,
                        &mut voxel_buffers.ssao,
                    )
                    .expect("failed to submit voxel SSAO");
                self.voxel_ctx
                    .shade
                    .submit(
                        &voxel_buffers.merge,
                        Some(&voxel_buffers.ssao),
                        &mut self.voxel_workspace.shade,
                        &mut voxel_buffers.shade,
                    )
                    .expect("failed to submit shaded rendering");
            }
        };

        self.gpu
            .copy(voxel_buffers.shade.output(), &mut voxel_buffers.read)
            .expect("failed to read voxel buffer");
        let mapped_image =
            self.gpu.map_image_async(&mut voxel_buffers.read).await;
        let image = mapped_image.image();
        let color = image.take().0;

        RgbaImage {
            color,
            size: image_size,
        }
    }

    fn get_voxel_buffers(
        &mut self,
        block: BlockIndex,
        image_size: VoxelSize,
    ) -> VoxelBuffers {
        self.voxel_buffers
            .remove(&(block, image_size))
            .unwrap_or_else(|| self.build_voxel_buffers(image_size))
    }

    fn build_voxel_buffers(&self, image_size: VoxelSize) -> VoxelBuffers {
        VoxelBuffers {
            render: self.voxel_ctx.render.buffers(image_size).unwrap(),
            merge: self.voxel_ctx.merge.buffers(image_size).unwrap(),
            shade: self.voxel_ctx.shade.buffers(image_size).unwrap(),
            ssao: self.voxel_ctx.ssao.buffers(image_size.into()).unwrap(),
            read: self.gpu.read_buffer("voxel read", image_size).unwrap(),
        }
    }

    fn store_voxel_buffers(
        &mut self,
        block: BlockIndex,
        image_size: VoxelSize,
        buf: VoxelBuffers,
    ) {
        self.voxel_buffers.insert((block, image_size), buf);
    }

    async fn render_pixel_f16(
        &mut self,
        vs: &ImageRenderSettings,
        pixel_buffers: &mut PixelBuffers,
        shapes: &mut RenderShapes,
    ) -> PixelImage {
        let image_size = self.submit_pixel(vs, pixel_buffers, shapes);

        let color = if let Some(c) = pixel_buffers.merge.output_color() {
            self.gpu
                .copy(c, &mut pixel_buffers.read_color_buffer)
                .expect("failed to read pixel color buffers");
            let mapped_color_image = self
                .gpu
                .map_image_async(&mut pixel_buffers.read_color_buffer)
                .await;
            let data = mapped_color_image.image().take().0;
            Some(<[[u8; 4]]>::ref_from_bytes(data.as_bytes()).unwrap().into())
        } else {
            None
        };

        // Do a compute pass to downsample the f32 buffer to f16
        let mut encoder = self.gpu.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("downfloat"),
            },
        );
        {
            // compute_pass scope
            let mut compute_pass = encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            // TODO this creates a bind group on each evaluation
            let bg =
                self.gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("downfloat bind group"),
                        layout: &self.pixel_downfloat_bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: pixel_buffers
                                    .merge
                                    .output_distance()
                                    .bind_active(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: pixel_buffers
                                    .distance_buffer_f16
                                    .as_entire_binding(),
                            },
                        ],
                    });
            compute_pass.set_bind_group(0, Some(&bg), &[]);
            compute_pass.set_pipeline(&self.pixel_downfloat_pipeline);
            let nx = (u64::from(image_size.width())
                * u64::from(image_size.height()))
            .div_ceil(2)
            .div_ceil(64);
            compute_pass.dispatch_workgroups(nx.try_into().unwrap(), 1, 1);
        }

        encoder.copy_buffer_to_buffer(
            &pixel_buffers.distance_buffer_f16,
            0,
            &pixel_buffers.read_distance_buffer_f16,
            0,
            pixel_buffers.distance_buffer_f16.size(),
        );
        self.gpu.queue.submit(std::iter::once(encoder.finish()));

        let distance = {
            let slice = pixel_buffers.read_distance_buffer_f16.slice(..);

            let (tx, rx) = flume::bounded(1);
            slice.map_async(wgpu::MapMode::Read, move |_| tx.send(()).unwrap());
            self.gpu
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            rx.recv_async().await.unwrap();
            let slice = slice.get_mapped_range().unwrap();

            // Strip any padding bytes (since this is a `f16` buffer but
            // requires 4-byte alignment for binding and mapping).
            let n = usize::try_from(image_size.width()).unwrap()
                * usize::try_from(image_size.height()).unwrap();
            <[Float16]>::ref_from_bytes(&slice[..]).unwrap()[..n].to_owned()
        };
        pixel_buffers.read_distance_buffer_f16.unmap();

        PixelImage {
            distance,
            color,
            size: image_size,
        }
    }

    fn get_pixel_buffers(
        &mut self,
        block: BlockIndex,
        image_size: ImageSize,
    ) -> PixelBuffers {
        self.pixel_buffers
            .remove(&(block, image_size))
            .unwrap_or_else(|| self.build_pixel_buffers(image_size))
    }

    fn build_pixel_buffers(&self, image_size: ImageSize) -> PixelBuffers {
        // TODO should we split this into f16 and f32 flavors?
        let f16_size = (u64::from(image_size.width())
            * u64::from(image_size.height())
            * std::mem::size_of::<Float16>() as u64)
            .next_multiple_of(4); // Round up for alignment here!
        PixelBuffers {
            render: self.pixel_ctx.render.buffers(image_size).unwrap(),
            merge: self.pixel_ctx.merge.buffers(image_size).unwrap(),
            distance_buffer_f16: self.gpu.device.create_buffer(
                &wgpu::BufferDescriptor {
                    label: Some("distance16"),
                    usage: wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                    size: f16_size,
                },
            ),
            read_distance_buffer_f16: self.gpu.device.create_buffer(
                &wgpu::BufferDescriptor {
                    label: Some("read distance16"),
                    usage: wgpu::BufferUsages::MAP_READ
                        | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                    size: f16_size,
                },
            ),
            read_color_buffer: self
                .gpu
                .read_buffer("pixel color", image_size)
                .unwrap(),
            read_distance_buffer_f32: self
                .gpu
                .read_buffer("pixel f32 distance", image_size)
                .unwrap(),
        }
    }

    fn store_pixel_buffers(
        &mut self,
        block: BlockIndex,
        image_size: ImageSize,
        buf: PixelBuffers,
    ) {
        self.pixel_buffers.insert((block, image_size), buf);
    }

    async fn render_pixel_f32(
        &mut self,
        vs: &ImageRenderSettings,
        pixel_buffers: &mut PixelBuffers,
        shapes: &mut RenderShapes,
    ) -> (Vec<RawDistancePixel>, Option<Vec<[u8; 4]>>) {
        self.submit_pixel(vs, pixel_buffers, shapes);
        let color = if let Some(c) = pixel_buffers.merge.output_color() {
            self.gpu
                .copy(c, &mut pixel_buffers.read_color_buffer)
                .expect("failed to read pixel color buffer");
            let mapped_color_image = self
                .gpu
                .map_image_async(&mut pixel_buffers.read_color_buffer)
                .await;
            let data = mapped_color_image.image().take().0;
            Some(<[[u8; 4]]>::ref_from_bytes(data.as_bytes()).unwrap().into())
        } else {
            None
        };

        self.gpu
            .copy(
                pixel_buffers.merge.output_distance(),
                &mut pixel_buffers.read_distance_buffer_f32,
            )
            .expect("failed to read f32 pixel distance buffer");
        let mapped_distance_image = self
            .gpu
            .map_image_async(&mut pixel_buffers.read_distance_buffer_f32)
            .await;
        let distance_img = mapped_distance_image.image();
        let distance = distance_img.take().0;
        (distance, color)
    }

    fn submit_pixel(
        &mut self,
        vs: &ImageRenderSettings,
        pixel_buffers: &mut PixelBuffers,
        shapes: &mut RenderShapes,
    ) -> ImageSize {
        let ImageRenderSettings { mode, view, .. } = vs;
        let image_size = vs.render_size();
        let world_to_model = view.world_to_model();

        // Render and accumulate every shape into merge buffers
        pixel_buffers.merge.reset();

        for s in shapes.shapes.iter() {
            self.pixel_ctx
                .render
                .submit(
                    s,
                    fidget::wgpu::pixel::render::Settings {
                        world_to_model,
                        pixel_perfect: matches!(mode, ViewMode2::Sdf),
                        z: 0.0,
                        vars: &Default::default(),
                    },
                    &mut self.pixel_workspace.render,
                    &mut pixel_buffers.render,
                )
                .expect("failed to submit pixel render");
            self.pixel_ctx
                .merge
                .submit(
                    &pixel_buffers.render,
                    fidget::wgpu::pixel::merge::Settings { remove_nans: true },
                    &mut self.pixel_workspace.merge,
                    &mut pixel_buffers.merge,
                )
                .expect("failed to submit pixel merge");
        }

        if let Some(color) = shapes.color.as_ref() {
            self.pixel_ctx
                .color
                .submit(
                    color,
                    fidget::wgpu::pixel::color::Settings {
                        z: 0.0,
                        world_to_model,
                        only_filled: true,
                        vars: &Default::default(),
                    },
                    &mut self.pixel_workspace.color,
                    &mut pixel_buffers.merge,
                )
                .expect("failed to submit color rendering");
        }

        image_size
    }

    fn end_frame(&mut self, active: HashSet<BlockIndex>) {
        self.voxel_buffers.retain(|k, _v| active.contains(&k.0));
        self.pixel_buffers.retain(|k, _v| active.contains(&k.0));
        self.shapes.retain(|k, _v| active.contains(&k.0));
    }
}
