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
    BlockIndex, Message, MessageGenSender, RenderViewReply,
    platform::Notify,
    view::{
        PixelImage, RgbaImage, ViewCanvas, ViewImage, ViewMode2, ViewMode3,
    },
    world::{Color, Scene},
};

use fidget::raster::pixel::RawDistancePixel;

use web_time::Instant;
use zerocopy::{FromBytes, IntoBytes};

#[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
pub(crate) type RenderFunction = fidget::jit::JitFunction;

#[cfg(any(target_arch = "wasm32", not(feature = "jit")))]
pub(crate) type RenderFunction = fidget::vm::VmFunction;

pub(crate) type RenderShape = fidget::shape::Shape<RenderFunction>;

/// State representing an in-progress render
///
/// This lives in the main thread; the work itself lives in [`RenderTask`] in
/// the worker pool.
pub struct RenderTaskHandle {
    settings: RenderSettings,
    level: usize,
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
        level: usize,
        tx: MessageGenSender<N>,
    ) -> RenderTaskHandle {
        let cancel = fidget::render::CancelToken::new();
        let start_time = Instant::now();
        let task = RenderTask {
            block,
            generation,
            settings: settings.clone(),
            level,
            start_time,
            cancel: cancel.clone(),
            reply: tx,
        };
        self.tx.send(task).expect("all render threads stopped");
        RenderTaskHandle {
            settings,
            cancel,
            level,
        }
    }
}

impl RenderTaskHandle {
    /// Checks whether the new settings are different from our settings
    ///
    /// This only returns `true` if `self.level != max_level`; we want to avoid
    /// interrupting max-level renders to preserve responsiveness.
    pub fn should_cancel(
        &self,
        other: &RenderSettings,
        max_level: usize,
    ) -> bool {
        let settings_changed = &self.settings != other;
        settings_changed && self.level != max_level
    }
}

/// Object representing a render task
pub struct RenderTask<N: Notify> {
    block: BlockIndex,
    generation: u64,
    settings: RenderSettings,
    level: usize,
    cancel: fidget::render::CancelToken,
    reply: MessageGenSender<N>,
    start_time: Instant,
}

/// Compares two distance pixels
///
/// Returns `true` if we should swap (i.e. replace `a` with `b`)
fn compare_distance_pixel(a: RawDistancePixel, b: RawDistancePixel) -> bool {
    // For inside pixels, prefer `b` over `a` so that the last image wins
    if b.inside() {
        true
    } else if a.inside() {
        false
    } else if let (Some(da), Some(db)) = (a.distance(), b.distance()) {
        // Outside pixels are only rendered in SDF mode, which is pixel-perfect
        // (so we should always have distance values).  In this case, we'll do a
        // true `min` for outside pixels, instead of the `b`-over-`a` logic
        // which is used for inside pixels
        db < da
    } else {
        // Otherwise, just prefer `b`; we are presumably in a non-SDF mode which
        // skips outside pixels anyways.
        true
    }
}

/// Settings for rendering an image
#[derive(Clone, PartialEq)]
pub enum RenderSettings {
    Image(ImageRenderSettings),
    Voxel(VoxelRenderSettings),
}

#[derive(Clone, PartialEq)]
pub struct ImageRenderSettings {
    scene: Scene, // TODO cloning scenes can be expensive
    mode: ViewMode2,
    view: fidget::gui::View2,
    size: fidget::render::ImageSize,
}

#[derive(Clone, PartialEq)]
pub struct VoxelRenderSettings {
    scene: Scene, // TODO move sceen to task?
    mode: ViewMode3,
    perspective: bool,
    view: fidget::gui::View3,
    size: fidget::render::VoxelSize,
}

impl RenderSettings {
    pub fn from_canvas(canvas: &ViewCanvas, scene: Scene) -> Self {
        match canvas {
            ViewCanvas::Canvas2 { canvas, mode } => {
                RenderSettings::Image(ImageRenderSettings {
                    scene,
                    view: canvas.view(),
                    size: canvas.image_size(),
                    mode: *mode,
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
                    size: fidget::render::VoxelSize::new(
                        size.width(),
                        size.height(),
                        // XXX select depth?
                        size.width().max(size.height()),
                    ),
                    mode: *mode,
                })
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Render worker, to be run in a thread (native) or Web Worker (web)
pub(crate) async fn render_worker<N: Notify>(
    rx: flume::Receiver<RenderTask<N>>,
) {
    let mut gpu = GpuWorker::new().await;
    while let Ok(task) = rx.recv_async().await {
        if let Some(data) = gpu.render(&task).await {
            task.reply.send(Message::RenderView(RenderViewReply {
                block: task.block,
                generation: task.generation,
                start_time: task.start_time,
                data,
                settings: task.settings,
            }))
        }
    }
}

struct GpuWorker {
    gpu: fidget::wgpu::Gpu,

    voxel_ctx: fidget::wgpu::voxel::Context,
    voxel_effects: fidget::wgpu::voxel::effects::Context,
    voxel_buffers: fidget::wgpu::voxel::Buffers,
    voxel_merge_buffers: fidget::wgpu::voxel::effects::MergeBuffers,
    voxel_ssao_buffers: fidget::wgpu::voxel::effects::SsaoBuffers,
    voxel_shade_buffers: fidget::wgpu::voxel::effects::ShadeBuffers,
    voxel_read_buffer: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::voxel::effects::ShadedImageTag,
    >,

    pixel_ctx: fidget::wgpu::pixel::Context,
    pixel_effects: fidget::wgpu::pixel::effects::Context,
    pixel_buffers: fidget::wgpu::pixel::Buffers,
    pixel_merge_buffers: fidget::wgpu::pixel::effects::MergeBuffers,
    pixel_read_distance_buffer: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::pixel::effects::PixelDistanceBufferTag,
    >,
    pixel_read_color_buffer: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::pixel::effects::PixelColorBufferTag,
    >,
}

impl GpuWorker {
    async fn new() -> Self {
        let gpu = fidget::wgpu::Gpu::init().await.unwrap();
        let voxel_ctx = fidget::wgpu::voxel::Context::new(&gpu);
        let voxel_effects = fidget::wgpu::voxel::effects::Context::new(&gpu);
        let voxel_shade_buffers = voxel_effects.shade_buffers();
        let voxel_buffers = voxel_ctx.buffers();
        let voxel_merge_buffers = voxel_effects.merge_buffers();
        let voxel_ssao_buffers = voxel_effects.ssao_buffers();
        let voxel_read_buffer = gpu.read_buffer("voxel read");

        let pixel_ctx = fidget::wgpu::pixel::Context::new(&gpu);
        let pixel_effects = fidget::wgpu::pixel::effects::Context::new(&gpu);
        let pixel_merge_buffers = pixel_effects.merge_buffers();
        let pixel_buffers = pixel_ctx.buffers();
        let pixel_read_color_buffer = gpu.read_buffer("pixel color read");
        let pixel_read_distance_buffer = gpu.read_buffer("pixel distance read");

        Self {
            gpu,
            voxel_shade_buffers,
            voxel_ctx,
            voxel_effects,
            voxel_buffers,
            voxel_read_buffer,
            voxel_merge_buffers,
            voxel_ssao_buffers,

            pixel_ctx,
            pixel_effects,
            pixel_buffers,
            pixel_merge_buffers,
            pixel_read_color_buffer,
            pixel_read_distance_buffer,
        }
    }

    async fn render<N: Notify>(
        &mut self,
        t: &RenderTask<N>,
    ) -> Option<ViewImage> {
        if t.cancel.is_cancelled() {
            return None;
        }
        match &t.settings {
            RenderSettings::Voxel(vs) => self.render_voxel(vs, t.level).await,
            RenderSettings::Image(rs) => self.render_pixel(rs, t.level).await,
        }
    }

    async fn render_voxel(
        &mut self,
        vs: &VoxelRenderSettings,
        level: usize,
    ) -> Option<ViewImage> {
        let VoxelRenderSettings {
            scene,
            mode,
            view,
            size,
            perspective,
        } = vs;
        // If this is our final rendering level, then do oversampling in
        // the Z direction for better rendering of edges.  XXX if you
        // change this, then you also need to edit `shaded.rs` to adjust
        // the `max_depth` passed into the shader.
        let scale = 1 << level;
        let bonus_z = if level == 0 { 2 } else { 1 };
        let image_size = fidget::render::VoxelSize::new(
            (size.width() / scale).max(1),
            (size.height() / scale).max(1),
            (size.depth() / scale).max(1) * bonus_z,
        );
        let z_scale = 2.0 / bonus_z as f32;
        let scale = nalgebra::Scale3::new(1.0, 1.0, z_scale);
        let mut world_to_model = view.world_to_model() * scale.to_homogeneous();
        if *perspective {
            *world_to_model.get_mut((3, 2)).unwrap() = 0.3 / bonus_z as f32;
        }
        let render_cfg = fidget::raster::voxel::RenderConfig {
            image_size,
            world_to_model,
        };

        // Render and accumulate every shape into merge buffers
        self.voxel_merge_buffers.reset();
        for s in &scene.shapes {
            let rs = s.tree.clone().into();
            // TODO cache and reuse shapes
            let shape =
                self.gpu.shape(&rs).expect("failed to get render shape");
            self.voxel_ctx
                .submit(&shape, &mut self.voxel_buffers, &render_cfg)
                .expect("failed to submit voxel render");
            self.voxel_effects
                .submit_merge(
                    self.voxel_buffers.output(),
                    true,
                    &mut self.voxel_merge_buffers,
                )
                .expect("failed to submit voxel merge");
        }
        if scene.shapes.iter().any(|c| c.color.is_some()) {
            // TODO cache and reuse colors
            let colors = scene
                .shapes
                .iter()
                .map(|t| {
                    t.color
                        .as_ref()
                        .map(|c| match c {
                            Color::Rgb([r, g, b]) => {
                                fidget::wgpu::ShapeColor::Rgb {
                                    // TODO(fidget) this is awkward, should we
                                    // also implement Into on &Tree?
                                    r: r.clone().into(),
                                    g: g.clone().into(),
                                    b: b.clone().into(),
                                }
                            }
                            Color::Hsl(..) => unimplemented!(),
                        })
                        .unwrap_or_else(|| {
                            let c =
                                || fidget::context::Tree::constant(1.0).into();
                            fidget::wgpu::ShapeColor::Rgb {
                                r: c(),
                                g: c(),
                                b: c(),
                            }
                        })
                })
                .collect::<Vec<_>>();
            let colors = self.gpu.color_buffers(&colors).unwrap();
            self.voxel_effects
                .submit_color(
                    &self.voxel_merge_buffers,
                    &world_to_model,
                    &colors,
                    &mut self.voxel_shade_buffers,
                )
                .expect("failed to submit color rendering");
        }

        match mode {
            ViewMode3::Heightmap => self
                .voxel_effects
                .submit_heightmap(
                    &self.voxel_merge_buffers,
                    &mut self.voxel_shade_buffers,
                )
                .expect("failed to submit shaded rendering"),
            ViewMode3::Shaded => {
                self.voxel_effects
                    .submit_ssao(
                        &self.voxel_merge_buffers,
                        &mut self.voxel_ssao_buffers,
                    )
                    .expect("failed to submit voxel SSAO");
                self.voxel_effects
                    .submit_shade(
                        &self.voxel_merge_buffers,
                        Some(&self.voxel_ssao_buffers),
                        &mut self.voxel_shade_buffers,
                    )
                    .expect("failed to submit shaded rendering");
            }
        };

        self.gpu.copy(
            self.voxel_shade_buffers.output(),
            &mut self.voxel_read_buffer,
        );
        let mapped_image =
            self.gpu.map_image_async(&mut self.voxel_read_buffer).await;
        let image = mapped_image.image();
        let color = image.take().0.into();

        let image = RgbaImage {
            view: *view,
            size: *size,
            level,
            color,
            mode: *mode,
        };
        Some(ViewImage::Voxel(image))
    }

    async fn render_pixel(
        &mut self,
        vs: &ImageRenderSettings,
        level: usize,
    ) -> Option<ViewImage> {
        let ImageRenderSettings {
            scene,
            mode,
            view,
            size,
        } = vs;
        // If this is our final rendering level, then do oversampling in
        // the Z direction for better rendering of edges.  XXX if you
        // change this, then you also need to edit `shaded.rs` to adjust
        // the `max_depth` passed into the shader.
        let scale = 1 << level;
        let image_size = fidget::render::ImageSize::new(
            (size.width() / scale).max(1),
            (size.height() / scale).max(1),
        );
        let world_to_model = view.world_to_model();
        let render_cfg = fidget::raster::pixel::RenderConfig {
            image_size,
            world_to_model,
            pixel_perfect: matches!(mode, ViewMode2::Sdf),
            z: 0.0,
        };

        // Render and accumulate every shape into merge buffers
        self.pixel_merge_buffers.reset();
        for s in &scene.shapes {
            let rs = s.tree.clone().into();
            // TODO cache and reuse shapes
            let shape =
                self.gpu.shape(&rs).expect("failed to get render shape");
            self.pixel_ctx
                .submit(&shape, &mut self.pixel_buffers, &render_cfg)
                .expect("failed to submit pixel render");
            self.pixel_effects
                .submit_merge(
                    self.pixel_buffers.output(),
                    true,
                    &mut self.pixel_merge_buffers,
                )
                .expect("failed to submit pixel merge");
        }

        if scene.shapes.iter().any(|c| c.color.is_some()) {
            // TODO cache and reuse colors
            let colors = scene
                .shapes
                .iter()
                .map(|t| {
                    t.color
                        .as_ref()
                        .map(|c| match c {
                            Color::Rgb([r, g, b]) => {
                                fidget::wgpu::ShapeColor::Rgb {
                                    // TODO(fidget) this is awkward, should we
                                    // also implement Into on &Tree?
                                    r: r.clone().into(),
                                    g: g.clone().into(),
                                    b: b.clone().into(),
                                }
                            }
                            Color::Hsl(..) => unimplemented!(),
                        })
                        .unwrap_or_else(|| {
                            let c =
                                || fidget::context::Tree::constant(1.0).into();
                            fidget::wgpu::ShapeColor::Rgb {
                                r: c(),
                                g: c(),
                                b: c(),
                            }
                        })
                })
                .collect::<Vec<_>>();
            let colors = self.gpu.color_buffers(&colors).unwrap();
            self.pixel_effects
                .submit_color(
                    &mut self.pixel_merge_buffers,
                    fidget::wgpu::pixel::effects::ColorSettings {
                        z: 0.0,
                        world_to_model,
                        only_filled: true,
                    },
                    &colors,
                )
                .expect("failed to submit color rendering");
        }

        let color = if let Some(c) = self.pixel_merge_buffers.output_color() {
            self.gpu.copy(c, &mut self.pixel_read_color_buffer);
            let mapped_color_image = self
                .gpu
                .map_image_async(&mut self.pixel_read_color_buffer)
                .await;
            let data = mapped_color_image.image().take().0;
            Some(<[[u8; 4]]>::ref_from_bytes(data.as_bytes()).unwrap().into())
        } else {
            None
        };

        self.gpu.copy(
            self.pixel_merge_buffers.output_distance(),
            &mut self.pixel_read_distance_buffer,
        );
        let mapped_distance_image = self
            .gpu
            .map_image_async(&mut self.pixel_read_distance_buffer)
            .await;
        let distance_img = mapped_distance_image.image();
        let distance =
            <[f32]>::ref_from_bytes(distance_img.take().0.as_bytes())
                .unwrap()
                .into();

        let image = PixelImage {
            distance,
            view: *view,
            size: *size,
            level,
            color,
            mode: *mode,
        };
        Some(ViewImage::Pixel(image))
    }
}
