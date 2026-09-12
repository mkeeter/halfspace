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
    view::{
        PixelImage, RgbaImage, ViewCanvas, ViewImage, ViewMode2, ViewMode3,
    },
    world::Scene,
};

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
            kind: TaskKind::Display {
                block,
                generation,
                level,
                start_time,
                reply: tx,
            },
            settings: settings.clone(),
            cancel: cancel.clone(),
        };
        self.tx.send(task).expect("all render threads stopped");
        RenderTaskHandle {
            settings,
            cancel,
            level,
        }
    }

    /// Begins a new export image rendering task in the worker pool
    pub(crate) fn export(
        &self,
        settings: RenderSettings,
        tx: MessageSender<N>,
    ) -> fidget::render::CancelToken {
        let cancel = fidget::render::CancelToken::new();
        let task = RenderTask {
            kind: TaskKind::Export { reply: tx },
            settings,
            cancel: cancel.clone(),
        };
        self.tx.send(task).expect("all render threads stopped");
        cancel
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
    settings: RenderSettings,
    cancel: fidget::render::CancelToken,
    kind: TaskKind<N>,
}

pub enum TaskKind<N: Notify> {
    Display {
        block: BlockIndex,
        generation: u64,
        reply: MessageGenSender<N>,
        start_time: Instant,
        level: usize,
    },
    Export {
        reply: MessageSender<N>,
    },
}

/// Settings for rendering an image
#[derive(Clone, PartialEq)]
pub enum RenderSettings {
    Image(ImageRenderSettings),
    Voxel(VoxelRenderSettings),
}

#[derive(Clone, PartialEq)]
pub struct ImageRenderSettings {
    pub scene: Scene,
    pub mode: ViewMode2,
    pub view: fidget::gui::View2,
    pub size: fidget::render::ImageSize,
}

#[derive(Clone, PartialEq)]
pub struct VoxelRenderSettings {
    pub scene: Scene,
    pub mode: ViewMode3,
    pub perspective: bool,
    pub view: fidget::gui::View3,
    pub size: fidget::render::VoxelSize,
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
    mut gpu: GpuWorker,
    rx: flume::Receiver<RenderTask<N>>,
    waker: flume::Sender<bool>,
) {
    while let Ok(task) = rx.recv_async().await {
        let _ = waker.try_send(true);
        match task.kind {
            TaskKind::Display {
                block,
                generation,
                start_time,
                level,
                reply,
            } => {
                if task.cancel.is_cancelled() {
                    continue;
                }
                let data = match &task.settings {
                    RenderSettings::Voxel(vs) => {
                        gpu.render_voxel(vs, level).await
                    }
                    RenderSettings::Image(rs) => {
                        gpu.render_pixel(rs, level).await
                    }
                };
                reply.send(Message::RenderView(RenderViewReply {
                    block,
                    generation,
                    start_time,
                    data,
                    settings: task.settings,
                }))
            }
            TaskKind::Export { reply } => {
                // Initial check for cancellation, in case there's a long queue
                // of things to render and we just now got to this one.
                if task.cancel.is_cancelled() {
                    reply.send(Message::ExportComplete(Err(
                        ExportError::Cancelled,
                    )));
                    continue;
                }

                // We can't pass cancellation through to the GPU, unfortunately,
                // so we'll kick off a rendering then check cancellation again
                // after we're done.
                let (data, image_size) = match &task.settings {
                    RenderSettings::Voxel(vs) => {
                        (gpu.render_voxel(vs, 0).await, vs.size.into())
                    }
                    RenderSettings::Image(rs) => {
                        (gpu.render_pixel(rs, 0).await, rs.size)
                    }
                };

                // Re-check cancellation.  This is a *little* silly, because I
                // expect image rendering to happen in ~milliseconds, but maybe
                // the user wants a gigantic export and will have time to hit
                // Cancel?
                if task.cancel.is_cancelled() {
                    reply.send(Message::ExportComplete(Err(
                        ExportError::Cancelled,
                    )));
                    continue;
                }

                let out = match data {
                    ViewImage::Pixel(px) => {
                        if let Some(c) = px.color {
                            c.as_bytes().to_vec()
                        } else {
                            px.distance
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
                    // TODO borrow in this case?
                    ViewImage::Voxel(im) => im.color.as_bytes().to_vec(),
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
                    Ok(()) => reply.send(Message::ExportComplete(Ok(bytes))),
                    Err(e) => {
                        reply.send(Message::ExportComplete(Err(e.into())))
                    }
                }
            }
        }
        let _ = waker.try_send(false);
    }
}

pub(crate) struct GpuWorker {
    pub gpu: fidget::wgpu::Gpu,

    voxel_ctx: fidget::wgpu::voxel::Context,
    voxel_effects: fidget::wgpu::voxel::effects::Context,
    voxel_workspace: fidget::wgpu::voxel::Workspace,
    voxel_merge_workspace: fidget::wgpu::voxel::effects::MergeWorkspace,
    voxel_ssao_workspace: fidget::wgpu::voxel::effects::SsaoWorkspace,
    voxel_shade_workspace: fidget::wgpu::voxel::effects::ShadeWorkspace,
    voxel_read_buffer: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::voxel::effects::ShadedImageTag,
    >,
    voxel_color_workspace: fidget::wgpu::voxel::effects::ColorWorkspace,

    pixel_ctx: fidget::wgpu::pixel::Context,
    pixel_effects: fidget::wgpu::pixel::effects::Context,
    pixel_workspace: fidget::wgpu::pixel::Workspace,
    pixel_merge_workspace: fidget::wgpu::pixel::effects::MergeWorkspace,
    pixel_read_distance_buffer: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::pixel::effects::PixelDistanceBufferTag,
    >,
    pixel_read_color_buffer: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::pixel::effects::PixelColorBufferTag,
    >,
    pixel_color_workspace: fidget::wgpu::pixel::effects::ColorWorkspace,
}

impl GpuWorker {
    pub(crate) async fn new() -> Self {
        let gpu = fidget::wgpu::Gpu::init().await.unwrap();
        let voxel_ctx = fidget::wgpu::voxel::Context::new(&gpu);
        let voxel_effects = fidget::wgpu::voxel::effects::Context::new(&gpu);
        let voxel_shade_workspace = voxel_effects.shade_workspace();
        let voxel_workspace = voxel_ctx.workspace();
        let voxel_merge_workspace = voxel_effects.merge_workspace();
        let voxel_ssao_workspace = voxel_effects.ssao_workspace();
        let voxel_read_buffer = gpu.read_buffer("voxel read");
        let voxel_color_workspace = voxel_effects.color_workspace();

        let pixel_ctx = fidget::wgpu::pixel::Context::new(&gpu);
        let pixel_effects = fidget::wgpu::pixel::effects::Context::new(&gpu);
        let pixel_merge_workspace = pixel_effects.merge_workspace();
        let pixel_workspace = pixel_ctx.workspace();
        let pixel_read_color_buffer = gpu.read_buffer("pixel color read");
        let pixel_read_distance_buffer = gpu.read_buffer("pixel distance read");
        let pixel_color_workspace = pixel_effects.color_workspace();

        Self {
            gpu,
            voxel_shade_workspace,
            voxel_ctx,
            voxel_effects,
            voxel_workspace,
            voxel_read_buffer,
            voxel_merge_workspace,
            voxel_ssao_workspace,
            voxel_color_workspace,

            pixel_ctx,
            pixel_effects,
            pixel_workspace,
            pixel_merge_workspace,
            pixel_read_color_buffer,
            pixel_read_distance_buffer,
            pixel_color_workspace,
        }
    }

    async fn render_voxel(
        &mut self,
        vs: &VoxelRenderSettings,
        level: usize,
    ) -> ViewImage {
        let VoxelRenderSettings {
            scene,
            mode,
            view,
            size,
            perspective,
        } = vs;
        // If this is our final rendering level, then do oversampling in
        // the Z direction for better rendering of edges.
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
        self.voxel_merge_workspace.reset();
        let merge_settings = fidget::wgpu::voxel::effects::MergeSettings {
            denoise: true,
            z_scale,
        };
        for s in scene.shapes.iter() {
            let rs = s.tree.clone().into();
            // TODO cache and reuse shapes
            let shape = fidget::wgpu::RenderShape::new(&rs)
                .expect("failed to get render shape");
            self.voxel_ctx
                .submit(&shape, &mut self.voxel_workspace, &render_cfg)
                .expect("failed to submit voxel render");
            self.voxel_effects
                .submit_merge(
                    self.voxel_workspace.output(),
                    merge_settings,
                    &mut self.voxel_merge_workspace,
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
                        .map(fidget::wgpu::color::ShapeColor::from)
                        .unwrap_or_else(|| {
                            let c =
                                || fidget::context::Tree::constant(1.0).into();
                            fidget::wgpu::color::ShapeColor::Rgb {
                                r: c(),
                                g: c(),
                                b: c(),
                            }
                        })
                })
                .collect::<Vec<_>>();
            let colors =
                fidget::wgpu::color::ShapeColorBuffers::new(&colors).unwrap();
            self.voxel_effects
                .submit_color(
                    &self.voxel_merge_workspace,
                    &world_to_model,
                    &colors,
                    &mut self.voxel_color_workspace,
                    &mut self.voxel_shade_workspace,
                )
                .expect("failed to submit color rendering");
        }

        match mode {
            ViewMode3::Heightmap => self
                .voxel_effects
                .submit_heightmap(
                    &self.voxel_merge_workspace,
                    &mut self.voxel_shade_workspace,
                )
                .expect("failed to submit shaded rendering"),
            ViewMode3::Shaded => {
                self.voxel_effects
                    .submit_ssao(
                        &self.voxel_merge_workspace,
                        &mut self.voxel_ssao_workspace,
                    )
                    .expect("failed to submit voxel SSAO");
                self.voxel_effects
                    .submit_shade(
                        &self.voxel_merge_workspace,
                        Some(&self.voxel_ssao_workspace),
                        &mut self.voxel_shade_workspace,
                    )
                    .expect("failed to submit shaded rendering");
            }
        };

        self.gpu.copy(
            self.voxel_shade_workspace.output(),
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
        ViewImage::Voxel(image)
    }

    async fn render_pixel(
        &mut self,
        vs: &ImageRenderSettings,
        level: usize,
    ) -> ViewImage {
        let ImageRenderSettings {
            scene,
            mode,
            view,
            size,
        } = vs;
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
        self.pixel_merge_workspace.reset();
        for s in scene.shapes.iter() {
            let rs = s.tree.clone().into();
            // TODO cache and reuse shapes
            let shape = fidget::wgpu::RenderShape::new(&rs)
                .expect("failed to get render shape");
            self.pixel_ctx
                .submit(&shape, &mut self.pixel_workspace, &render_cfg)
                .expect("failed to submit pixel render");
            self.pixel_effects
                .submit_merge(
                    self.pixel_workspace.output(),
                    true,
                    &mut self.pixel_merge_workspace,
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
                        .map(fidget::wgpu::color::ShapeColor::from)
                        .unwrap_or_else(|| {
                            let c =
                                || fidget::context::Tree::constant(1.0).into();
                            fidget::wgpu::color::ShapeColor::Rgb {
                                r: c(),
                                g: c(),
                                b: c(),
                            }
                        })
                })
                .collect::<Vec<_>>();
            let colors =
                fidget::wgpu::color::ShapeColorBuffers::new(&colors).unwrap();
            self.pixel_effects
                .submit_color(
                    &mut self.pixel_merge_workspace,
                    fidget::wgpu::pixel::effects::ColorSettings {
                        z: 0.0,
                        world_to_model,
                        only_filled: true,
                    },
                    &colors,
                    &mut self.pixel_color_workspace,
                )
                .expect("failed to submit color rendering");
        }

        let color = if let Some(c) = self.pixel_merge_workspace.output_color() {
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
            self.pixel_merge_workspace.output_distance(),
            &mut self.pixel_read_distance_buffer,
        );
        let mapped_distance_image = self
            .gpu
            .map_image_async(&mut self.pixel_read_distance_buffer)
            .await;
        let distance_img = mapped_distance_image.image();
        let distance = distance_img.take().0.into();

        let image = PixelImage {
            distance,
            view: *view,
            size: *size,
            level,
            color,
            mode: *mode,
        };
        ViewImage::Pixel(image)
    }
}
