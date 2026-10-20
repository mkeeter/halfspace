use crate::{
    BlockIndex, MessageReceiver, RenderViewReply, ViewResponse,
    gui::{CAMERA, WARN},
    painters::WgpuResources,
    platform::Notify,
    render::{
        Float16, ImageRenderSettings, RenderImage, RenderSettings,
        RenderTaskHandle, RenderWorkerPool, VoxelRenderSettings,
    },
    state,
    state::ViewState,
    world::Scene,
};

pub use state::{ViewMode2, ViewMode3};

use egui_wgpu::wgpu;
use web_time::Duration;

/// State associated with a given view in the GUI
///
/// Each block may have 0 or 1 views.  Views are persistent even when closed;
/// they're deleted when their block is deleted.
pub struct ViewData {
    /// Handle to a render task, running in a thread pool
    pub task: Option<RenderTaskHandle>,

    /// Interaction canvas
    pub canvas: ViewCanvas,

    /// Current image
    image: Option<ViewImage>,

    /// Initial render depth, used to render faster
    start_level: usize,

    /// Monotonic counter to identify the most recent task
    generation: u64,
}

/// Tuple of data to be loaded and its corresponding WGPU texture
pub struct TextureData<T> {
    /// Image data to copy to the texture
    ///
    /// If this is empty, then the texture is presumed to already be populated
    pub data: Vec<T>,
    pub texture: wgpu::Texture,
}

impl<T> TextureData<T> {
    pub fn take(&mut self) -> Self {
        TextureData {
            data: std::mem::take(&mut self.data),
            texture: self.texture.clone(),
        }
    }
}

impl ViewData {
    /// Returns a characteristic transform matrix for this view
    ///
    /// The scale should be applied to mouse motion in pixels
    pub fn characteristic_matrix(&self) -> nalgebra::Matrix4<f32> {
        match self.canvas {
            ViewCanvas::Canvas2 { canvas, .. } => {
                let m = canvas.view().world_to_model()
                    * canvas.image_size().screen_to_world();
                #[rustfmt::skip]
                let mat = nalgebra::Matrix4::new(
                    m[(0, 0)], m[(0, 1)], 0.0, m[(0, 2)],
                    m[(1, 0)], m[(1, 1)], 0.0, m[(1, 2)],
                    0.0,        0.0,      1.0, 0.0,
                    m[(2, 0)], m[(2, 1)], 0.0, m[(2,2)],
                );
                mat
            }
            ViewCanvas::Canvas3 { canvas, .. } => {
                canvas.view().world_to_model()
                    * canvas.image_size().screen_to_world()
            }
        }
    }
}

impl From<ViewCanvas> for ViewData {
    fn from(canvas: ViewCanvas) -> Self {
        Self {
            task: None,
            canvas,
            image: None,
            start_level: 0,
            generation: 0,
        }
    }
}

/// State associated with the canvas (for interactions)
#[derive(Copy, Clone)]
pub enum ViewCanvas {
    Canvas2 {
        canvas: fidget::gui::Canvas2,
        mode: ViewMode2,
    },
    Canvas3 {
        canvas: fidget::gui::Canvas3,
        perspective: bool,
        mode: ViewMode3,
    },
}

impl From<&ViewCanvas> for state::ViewState {
    fn from(v: &ViewCanvas) -> state::ViewState {
        match v {
            ViewCanvas::Canvas2 { canvas, mode } => {
                let (view, size) = canvas.components();
                let (center, scale) = view.components();
                ViewState::View2 {
                    mode: *mode,
                    center,
                    scale,
                    width: size.width(),
                    height: size.height(),
                }
            }
            ViewCanvas::Canvas3 {
                canvas,
                perspective,
                mode,
            } => {
                let (view, size) = canvas.components();
                let (center, scale, yaw, pitch) = view.components();
                ViewState::View3 {
                    mode: *mode,
                    center,
                    scale,
                    yaw,
                    pitch,
                    width: size.width(),
                    height: size.height(),
                    depth: size.depth(),
                    perspective: *perspective,
                }
            }
        }
    }
}

impl From<ViewState> for ViewCanvas {
    fn from(v: ViewState) -> Self {
        match v {
            // Use dummy sizes for the canvas; they'll be updated on the first
            // drawing pass.
            ViewState::View2 {
                mode,
                center,
                scale,
                width,
                height,
            } => {
                let canvas = fidget::gui::Canvas2::from_components(
                    fidget::gui::View2::from_components(center, scale),
                    fidget::render::ImageSize::new(width, height),
                );
                Self::Canvas2 { canvas, mode }
            }
            ViewState::View3 {
                mode,
                center,
                scale,
                yaw,
                pitch,
                width,
                height,
                depth,
                perspective,
            } => {
                let canvas = fidget::gui::Canvas3::from_components(
                    fidget::gui::View3::from_components(
                        center, scale, yaw, pitch,
                    ),
                    fidget::render::VoxelSize::new(width, height, depth),
                );
                Self::Canvas3 {
                    canvas,
                    mode,
                    perspective,
                }
            }
        }
    }
}

/// Rendered image(s) to be drawn, along with the settings that generated it
///
/// The `Vec<T>` fields contain image data which should be copied to the
/// `wgpu::Texture` field when rendered.  As part of copying, the `Vec` is
/// emptied out; an empty `Vec` indicates that the texture is valid and can be
/// used for rendering immediately.
pub enum ViewImage {
    Pixel {
        distance: TextureData<Float16>,
        color: Option<TextureData<[u8; 4]>>,
        uniform_buffer: wgpu::Buffer,
        bind_group: wgpu::BindGroup,
        settings: ImageRenderSettings,
    },
    Voxel {
        /// Pre-baked color
        ///
        /// This is greyscale for a heightmap rendering, or RGB for shaded
        color: TextureData<u32>,
        bind_group: wgpu::BindGroup,
        uniform_buffer: wgpu::Buffer,
        settings: VoxelRenderSettings,
    },
}

impl ViewImage {
    fn matches(&self, settings: &RenderSettings) -> bool {
        match (self, settings) {
            (ViewImage::Voxel { settings, .. }, RenderSettings::Voxel(vs)) => {
                settings.matches(vs)
            }
            (ViewImage::Pixel { settings, .. }, RenderSettings::Image(vs)) => {
                settings.matches(vs)
            }
            _ => false,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ViewCanvasType {
    Bitfield,
    Sdf,
    Heightmap,
    Shaded,
}

impl ViewData {
    pub fn new(image_size: fidget::render::ImageSize) -> Self {
        Self {
            task: None,
            canvas: ViewCanvas::Canvas2 {
                canvas: fidget::gui::Canvas2::new(
                    fidget::render::ImageSize::new(
                        image_size.width(),
                        image_size.height(),
                    ),
                ),
                mode: ViewMode2::Sdf,
            },
            image: None,
            generation: 0,
            start_level: 0,
        }
    }

    /// Callback when a render task is complete
    pub fn update<N: Notify>(
        &mut self,
        r: RenderViewReply,
        rx: &MessageReceiver<N>,
        render_pool: &RenderWorkerPool<N>,
        device: &wgpu::Device,
        resources: &WgpuResources,
    ) {
        const TARGET_RENDER_TIME: Duration = Duration::from_millis(33);
        const MAX_LEVEL: usize = 10;
        let render_time = r.start_time.elapsed();

        // Adjust self.start_level to hit a render time target, with some
        // hysteresis to avoid bouncing between levels
        let level = r.image.level();
        if level == 0 {
            println!("rendered {:?} in {:?}", r.block, render_time);
        }
        if level == self.start_level {
            if render_time > TARGET_RENDER_TIME && level < MAX_LEVEL {
                self.start_level += 1;
            } else if render_time < TARGET_RENDER_TIME * 3 / 4 {
                self.start_level = self.start_level.saturating_sub(1);
            }
        }
        if r.generation == self.generation {
            let _ = self.task.take();
            if let Some(next) = level.checked_sub(1) {
                self.spawn_render_task(
                    r.block,
                    r.image.settings().with_level(next),
                    rx,
                    render_pool,
                );
            }
            let prev_image = self.image.take();
            self.image = Some(match r.image {
                RenderImage::Voxel { image, settings } => {
                    let texture_size = wgpu::Extent3d {
                        width: image.size.width(),
                        height: image.size.height(),
                        depth_or_array_layers: 1,
                    };

                    // If we have a previous image, then reuse its texture (if
                    // the size matches).  If not, then recreate the texture,
                    // view, and bind group.
                    let (texture, uniform_buffer) =
                        if let Some(ViewImage::Voxel {
                            color,
                            uniform_buffer,
                            bind_group,
                            ..
                        }) = prev_image
                        {
                            let texture =
                                if color.texture.size() == texture_size {
                                    Some(color.texture)
                                } else {
                                    None
                                };
                            (texture.map(|t| (t, bind_group)), uniform_buffer)
                        } else {
                            let uniform_buffer =
                                device.create_buffer(&wgpu::BufferDescriptor {
                                    label: Some("rgba uniform buffer"),
                                    size: std::mem::size_of::<
                                        crate::painters::RgbaUniforms,
                                    >(
                                    )
                                        as u64,
                                    mapped_at_creation: false,
                                    usage: wgpu::BufferUsages::UNIFORM
                                        | wgpu::BufferUsages::COPY_DST,
                                });
                            (None, uniform_buffer)
                        };

                    let (texture, bind_group) = texture.unwrap_or_else(|| {
                        let tex =
                            device.create_texture(&wgpu::TextureDescriptor {
                                label: Some("rgba texture"),
                                size: texture_size,
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format: wgpu::TextureFormat::Rgba8Unorm,
                                usage: wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::COPY_DST,
                                view_formats: &[],
                            });
                        // The texture view is reference-counted, so it's kept
                        // alive by the bind group below.
                        let rgba_texture_view =
                            tex.create_view(&Default::default());
                        let rgba_resources = resources.rgba();
                        let bind_group =
                            device
                                .create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("rgba bind group"),
                                layout: &rgba_resources.bind_group_layout,
                                entries: &[
                                    wgpu::BindGroupEntry {
                                        binding: 0,
                                        resource:
                                            wgpu::BindingResource::TextureView(
                                                &rgba_texture_view,
                                            ),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 1,
                                        resource:
                                            wgpu::BindingResource::Sampler(
                                                &rgba_resources.rgba_sampler,
                                            ),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 2,
                                        resource: uniform_buffer
                                            .as_entire_binding(),
                                    },
                                ],
                            });
                        (tex, bind_group)
                    });
                    ViewImage::Voxel {
                        color: TextureData {
                            data: image.color,
                            texture,
                        },
                        bind_group,
                        uniform_buffer,
                        settings,
                    }
                }
                RenderImage::Pixel { image, settings } => {
                    // Reuse textures if possible
                    let texture_size = wgpu::Extent3d {
                        width: image.size.width(),
                        height: image.size.height(),
                        depth_or_array_layers: 1,
                    };
                    let (
                        distance_tex,
                        mut color_tex,
                        mut bind_group,
                        uniform_buffer,
                    ) = if let Some(ViewImage::Pixel {
                        distance,
                        color,
                        uniform_buffer,
                        bind_group,
                        ..
                    }) = prev_image
                    {
                        // Distance and color textures are always the same
                        // dimensions, but the color texture is not always
                        // present, so we'll check the distance tex size.
                        let valid = distance.texture.size() == texture_size;
                        (
                            valid.then_some(distance.texture),
                            valid.then_some(color.map(|c| c.texture)).flatten(),
                            valid.then_some(bind_group),
                            uniform_buffer,
                        )
                    } else {
                        let uniform_buffer =
                            device.create_buffer(&wgpu::BufferDescriptor {
                                label: Some("pixel uniform buffer"),
                                size: std::mem::size_of::<
                                    crate::painters::SdfUniforms,
                                >()
                                    as u64,
                                mapped_at_creation: false,
                                usage: wgpu::BufferUsages::UNIFORM
                                    | wgpu::BufferUsages::COPY_DST,
                            });
                        (None, None, None, uniform_buffer)
                    };

                    // We'll now go through and make sure that all of the
                    // relevant textures are present.  If any of them change,
                    // then we'll clear `bind_group` to `None`, so that we can
                    // regenerate it with new bindings further below.

                    let distance_tex = distance_tex.unwrap_or_else(|| {
                        device.create_texture(&wgpu::TextureDescriptor {
                            label: Some("sdf distance texture"),
                            size: texture_size,
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::R16Float,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING
                                | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        })
                    });
                    if image.color.is_none() {
                        if color_tex.is_some() {
                            bind_group = None;
                        }
                        color_tex = None;
                    } else if color_tex.is_none() {
                        bind_group = None;
                        color_tex = Some(device.create_texture(
                            &wgpu::TextureDescriptor {
                                label: Some("sdf color texture"),
                                size: texture_size,
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format: wgpu::TextureFormat::Rgba8Unorm,
                                usage: wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::COPY_DST,
                                view_formats: &[],
                            },
                        ));
                    }

                    let sdf_resources = resources.sdf();
                    let color_tex = color_tex.unwrap_or_else(|| {
                        sdf_resources.dummy_color_texture.clone()
                    });
                    let bind_group = bind_group.unwrap_or_else(|| {
                        let distance_texture_view =
                            distance_tex.create_view(&Default::default());
                        let color_texture_view =
                            color_tex.create_view(&Default::default());
                        device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("sdf bind group"),
                            layout: &sdf_resources.bind_group_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource:
                                        wgpu::BindingResource::TextureView(
                                            &distance_texture_view,
                                        ),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::Sampler(
                                        &sdf_resources.distance_sampler,
                                    ),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 2,
                                    resource:
                                        wgpu::BindingResource::TextureView(
                                            &color_texture_view,
                                        ),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 3,
                                    resource: wgpu::BindingResource::Sampler(
                                        &sdf_resources.color_sampler,
                                    ),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 4,
                                    resource: uniform_buffer
                                        .as_entire_binding(),
                                },
                            ],
                        })
                    });

                    ViewImage::Pixel {
                        distance: TextureData {
                            data: image.distance,
                            texture: distance_tex,
                        },
                        color: image.color.map(|data| TextureData {
                            data,
                            texture: color_tex,
                        }),
                        settings,
                        uniform_buffer,
                        bind_group,
                    }
                }
            });
        }
    }

    /// Bumps `self.generation` by 1 and spawns a new render task
    fn spawn_render_task<N: Notify>(
        &mut self,
        block: BlockIndex,
        settings: RenderSettings,
        rx: &MessageReceiver<N>,
        render_pool: &RenderWorkerPool<N>,
    ) {
        self.generation += 1;
        self.task = Some(render_pool.spawn(
            block,
            self.generation,
            settings,
            rx.sender_with_gen(),
        ));
    }

    /// Gets the image, kicking off new render jobs if needed
    ///
    /// This should be called in the main GUI loop while visiting the editor
    /// tabs, when images are needed.
    pub(crate) fn image<N: Notify>(
        &mut self,
        block: BlockIndex,
        scene: Scene,
        rx: &MessageReceiver<N>,
        render_pool: &RenderWorkerPool<N>,
    ) -> Option<&mut ViewImage> {
        // Build settings assuming that we're starting from a max-level render
        let settings =
            RenderSettings::from_canvas(&self.canvas, scene, self.start_level);

        // If the image settings have changed, then start a new render, unless
        // the task is a max-level  (i.e. lowest-resolution) render, to preserve
        // responsiveness
        if self
            .task
            .as_ref()
            .is_some_and(|prev| prev.should_cancel(&settings))
        {
            self.task = None; // dropping the task cancels it
        }

        // If we don't have a task (either because it's done or it was cancelled
        // above), then start a new task if our settings don't match the
        // existing image settings.
        if self.task.is_none()
            && self
                .image
                .as_ref()
                .is_none_or(|image| !image.matches(&settings))
        {
            self.spawn_render_task(block, settings, rx, render_pool);
        }
        self.image.as_mut()
    }

    pub fn prev_image(&mut self) -> Option<&mut ViewImage> {
        self.image.as_mut()
    }
}

pub fn edit_button(
    ui: &mut egui::Ui,
    index: BlockIndex,
    entry: &mut ViewData,
    size: fidget::render::ImageSize,
) -> ViewResponse {
    let mut out = ViewResponse::empty();
    // Pop-up box to change render settings
    let initial_tag = match &entry.canvas {
        ViewCanvas::Canvas2 {
            mode: ViewMode2::Bitfield,
            ..
        } => ViewCanvasType::Bitfield,
        ViewCanvas::Canvas2 {
            mode: ViewMode2::Sdf,
            ..
        } => ViewCanvasType::Sdf,
        ViewCanvas::Canvas3 {
            mode: ViewMode3::Heightmap,
            ..
        } => ViewCanvasType::Heightmap,
        ViewCanvas::Canvas3 {
            mode: ViewMode3::Shaded,
            ..
        } => ViewCanvasType::Shaded,
    };
    let mut tag = initial_tag;
    let mut reset_camera = false;
    let perspective =
        if let ViewCanvas::Canvas3 { perspective, .. } = &mut entry.canvas {
            Some(perspective)
        } else {
            None
        };
    egui::ComboBox::from_id_salt(index.id().with("view_editor"))
        .selected_text(CAMERA)
        .width(0.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut tag,
                ViewCanvasType::Bitfield,
                "2D bitfield",
            );
            ui.selectable_value(&mut tag, ViewCanvasType::Sdf, "2D SDF");
            ui.separator();
            ui.selectable_value(
                &mut tag,
                ViewCanvasType::Heightmap,
                "3D heightmap",
            );
            ui.selectable_value(&mut tag, ViewCanvasType::Shaded, "3D shaded");
            ui.separator();
            if let Some(p) = perspective {
                ui.checkbox(p, "Perspective");
                ui.separator();
            }
            if ui.button("Reset camera").clicked() {
                reset_camera = true;
            }
        });
    // If we've edited the canvas tag, then update it in the entry
    if tag != initial_tag {
        out |= ViewResponse::REDRAW;
        let mut next_canvas = match tag {
            ViewCanvasType::Sdf | ViewCanvasType::Bitfield => {
                ViewCanvas::Canvas2 {
                    canvas: fidget::gui::Canvas2::new(size),
                    mode: match tag {
                        ViewCanvasType::Sdf => ViewMode2::Sdf,
                        ViewCanvasType::Bitfield => ViewMode2::Bitfield,
                        _ => unreachable!("invalid 2d canvas tag {tag:?}"),
                    },
                }
            }
            ViewCanvasType::Heightmap | ViewCanvasType::Shaded => {
                let size = fidget::render::VoxelSize::new(
                    size.width(),
                    size.height(),
                    size.width().max(size.height()), // XXX select depth?
                );
                ViewCanvas::Canvas3 {
                    canvas: fidget::gui::Canvas3::new(size),
                    mode: match tag {
                        ViewCanvasType::Heightmap => ViewMode3::Heightmap,
                        ViewCanvasType::Shaded => ViewMode3::Shaded,
                        _ => unreachable!("invalid 3d canvas tag {tag:?}"),
                    },
                    perspective: false,
                }
            }
        };
        match (&mut next_canvas, &mut entry.canvas) {
            (
                ViewCanvas::Canvas2 {
                    canvas: next_canvas,
                    ..
                },
                ViewCanvas::Canvas2 {
                    canvas: prev_canvas,
                    ..
                },
            ) => std::mem::swap(next_canvas, prev_canvas),
            (
                ViewCanvas::Canvas3 {
                    canvas: next_canvas,
                    perspective: next_perspective,
                    ..
                },
                ViewCanvas::Canvas3 {
                    canvas: prev_canvas,
                    perspective: prev_perspective,
                    ..
                },
            ) => {
                std::mem::swap(next_canvas, prev_canvas);
                std::mem::swap(next_perspective, prev_perspective);
            }
            _ => (), // TODO do some swapping if we do 2D <-> 3D?
        }
        entry.canvas = next_canvas;
    }
    if reset_camera {
        match &mut entry.canvas {
            ViewCanvas::Canvas2 { canvas, .. } => {
                *canvas = fidget::gui::Canvas2::new(canvas.image_size());
                out |= ViewResponse::REDRAW;
            }
            ViewCanvas::Canvas3 { canvas, .. } => {
                *canvas = fidget::gui::Canvas3::new(canvas.image_size());
                out |= ViewResponse::REDRAW;
            }
        }
    }
    out
}
/// Manually draw a backdrop indicating that the view is invalid
pub fn fallback_ui(
    ui: &mut egui::Ui,
    index: BlockIndex,
    entry: Option<&mut ViewData>,
    size: fidget::render::ImageSize,
    inner_text: &str,
    error_text: Option<&str>,
) -> ViewResponse {
    let mut out = ViewResponse::empty();

    let style = ui.style();
    let painter = ui.painter();

    let mut t = style.text_styles[&egui::TextStyle::Heading].clone();
    t.size *= 2.0;
    let layout = painter.layout(
        inner_text.to_owned(),
        t,
        style.visuals.widgets.noninteractive.text_color(),
        f32::INFINITY,
    );
    let rect = painter.clip_rect();
    let text_corner = rect.center() - layout.size() / 2.0;
    painter.rect_filled(rect, 0.0, style.visuals.panel_fill);
    painter.galley(text_corner, layout, egui::Color32::BLACK);

    if let Some(error_text) = error_text {
        ui.painter().rect_stroke(
            rect,
            0.0,
            egui::Stroke {
                width: 4.0,
                color: ui.style().visuals.error_fg_color,
            },
            egui::StrokeKind::Inside,
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            let r = ui
                .add(
                    egui::Label::new(
                        egui::RichText::new(WARN)
                            .color(egui::Color32::WHITE)
                            .background_color(
                                ui.style().visuals.error_fg_color,
                            ),
                    )
                    .sense(egui::Sense::CLICK),
                )
                .on_hover_ui(|ui| {
                    ui.label(error_text);
                });
            if r.clicked() {
                out |= ViewResponse::FOCUS_ERR;
            }
            if let Some(entry) = entry {
                ui.with_layout(
                    egui::Layout::left_to_right(egui::Align::TOP),
                    |ui| {
                        out |= edit_button(ui, index, entry, size);
                    },
                );
            }
        });
    } else if let Some(entry) = entry {
        out |= edit_button(ui, index, entry, size);
    }
    out
}
