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
        Float16, PixelImage, RgbaImage, ViewCanvas, ViewImage, ViewMode2,
        ViewMode3,
    },
    world::Scene,
};

use egui_wgpu::wgpu;
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
                        ViewImage::Voxel(gpu.render_voxel(vs, level).await)
                    }
                    RenderSettings::Image(rs) => {
                        ViewImage::Pixel(gpu.render_pixel_f16(rs, level).await)
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
                enum ExportImage {
                    Rgba(RgbaImage),
                    Distance {
                        distance: Vec<RawDistancePixel>,
                        color: Option<Vec<[u8; 4]>>,
                    },
                }
                let (data, image_size) = match &task.settings {
                    RenderSettings::Voxel(vs) => (
                        ExportImage::Rgba(gpu.render_voxel(vs, 0).await),
                        vs.size.into(),
                    ),
                    RenderSettings::Image(rs) => {
                        let (distance, color) =
                            gpu.render_pixel_f32(rs, 0).await;
                        (ExportImage::Distance { distance, color }, rs.size)
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
    pixel_distance_buffer_f16: fidget::wgpu::buf::FlexBuffer<Float16BufferTag>,
    pixel_read_distance_buffer_f16:
        fidget::wgpu::buf::ReadBuffer<Float16BufferTag>,
    pixel_read_distance_buffer_f32: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::pixel::effects::PixelDistanceBufferTag,
    >,
    pixel_read_color_buffer: fidget::wgpu::buf::ReadBuffer<
        fidget::wgpu::pixel::effects::PixelColorBufferTag,
    >,
    pixel_color_workspace: fidget::wgpu::pixel::effects::ColorWorkspace,
    pixel_downfloat_pipeline: wgpu::ComputePipeline,
    pixel_downfloat_bind_group_layout: wgpu::BindGroupLayout,
}

pub struct Float16BufferTag;
impl fidget::wgpu::buf::BufferTag for Float16BufferTag {
    type T = Float16;
    type S = fidget::render::ImageSize;
    fn usage() -> u32 {
        wgpu::BufferUsages::STORAGE.bits() | wgpu::BufferUsages::COPY_SRC.bits()
    }
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
        let pixel_distance_buffer_f16 = fidget::wgpu::buf::FlexBuffer::new(
            &gpu.device,
            "distance16",
            64.into(),
        )
        .expect("could not build pixel distance read buffer");
        let pixel_read_distance_buffer_f32 =
            gpu.read_buffer("pixel distance read");
        let pixel_read_distance_buffer_f16 =
            gpu.read_buffer("pixel distance read");
        let pixel_color_workspace = pixel_effects.color_workspace();

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
            pixel_read_distance_buffer_f16,
            pixel_read_distance_buffer_f32,
            pixel_distance_buffer_f16,
            pixel_downfloat_pipeline,
            pixel_downfloat_bind_group_layout,
            pixel_color_workspace,
        }
    }

    async fn render_voxel(
        &mut self,
        vs: &VoxelRenderSettings,
        level: usize,
    ) -> RgbaImage {
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
        for s in scene.render_shapes.iter() {
            self.voxel_ctx
                .submit(s, &mut self.voxel_workspace, &render_cfg)
                .expect("failed to submit voxel render");
            self.voxel_effects
                .submit_merge(
                    self.voxel_workspace.output(),
                    merge_settings,
                    &mut self.voxel_merge_workspace,
                )
                .expect("failed to submit voxel merge");
        }
        if let Some(colors) = scene.render_color.as_ref() {
            self.voxel_effects
                .submit_color(
                    &self.voxel_merge_workspace,
                    &world_to_model,
                    colors,
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

        RgbaImage {
            view: *view,
            size: *size,
            level,
            color,
            mode: *mode,
        }
    }

    async fn render_pixel_f16(
        &mut self,
        vs: &ImageRenderSettings,
        level: usize,
    ) -> PixelImage {
        self.submit_pixel(vs, level);

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
            self.pixel_distance_buffer_f16
                .grow_to_fit(&self.gpu.device, vs.size)
                .unwrap();
            // TODO this creates a bind group on each evaluation
            let bg =
                self.gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("merge bind group"),
                        layout: &self.pixel_downfloat_bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: self
                                    .pixel_workspace
                                    .output()
                                    .bind_active(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: self
                                    .pixel_distance_buffer_f16
                                    .bind_active(),
                            },
                        ],
                    });
            compute_pass.set_bind_group(0, Some(&bg), &[]);
            compute_pass.set_pipeline(&self.pixel_downfloat_pipeline);
            let nx = (u64::from(vs.size.width()) * u64::from(vs.size.height()))
                .div_ceil(2)
                .div_ceil(64);
            compute_pass.dispatch_workgroups(nx.try_into().unwrap(), 1, 1);
        }
        self.gpu.queue.submit(std::iter::once(encoder.finish()));
        self.gpu.copy(
            &self.pixel_distance_buffer_f16,
            &mut self.pixel_read_distance_buffer_f16,
        );
        let mapped_distance_image = self
            .gpu
            .map_image_async(&mut self.pixel_read_distance_buffer_f16)
            .await;
        let distance_img = mapped_distance_image.image();
        let distance = distance_img.take().0.into();

        PixelImage {
            distance,
            view: vs.view,
            size: vs.size,
            level,
            color,
            mode: vs.mode,
        }
    }

    async fn render_pixel_f32(
        &mut self,
        vs: &ImageRenderSettings,
        level: usize,
    ) -> (Vec<RawDistancePixel>, Option<Vec<[u8; 4]>>) {
        self.submit_pixel(vs, level);

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
            &mut self.pixel_read_distance_buffer_f32,
        );
        let mapped_distance_image = self
            .gpu
            .map_image_async(&mut self.pixel_read_distance_buffer_f32)
            .await;
        let distance_img = mapped_distance_image.image();
        let distance = distance_img.take().0;
        (distance, color)
    }

    fn submit_pixel(&mut self, vs: &ImageRenderSettings, level: usize) {
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
        for s in scene.render_shapes.iter() {
            self.pixel_ctx
                .submit(s, &mut self.pixel_workspace, &render_cfg)
                .expect("failed to submit pixel render");
            self.pixel_effects
                .submit_merge(
                    self.pixel_workspace.output(),
                    true,
                    &mut self.pixel_merge_workspace,
                )
                .expect("failed to submit pixel merge");
        }

        if let Some(color) = scene.render_color.as_ref() {
            self.pixel_effects
                .submit_color(
                    &mut self.pixel_merge_workspace,
                    fidget::wgpu::pixel::effects::ColorSettings {
                        z: 0.0,
                        world_to_model,
                        only_filled: true,
                    },
                    color,
                    &mut self.pixel_color_workspace,
                )
                .expect("failed to submit color rendering");
        }
    }
}
