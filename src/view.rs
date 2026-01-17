use crate::{
    BlockIndex, MessageGenSender, ViewResponse,
    gui::{CAMERA, WARN},
    platform::Notify,
    render::{RenderSettings, RenderTask},
    state,
    state::ViewState,
    world::Scene,
};
use std::sync::Arc;

pub use state::{ViewMode2, ViewMode3};
use web_time::Duration;

/// State associated with a given view in the GUI
///
/// Each block may have 0 or 1 views.  Views are persistent even when closed;
/// they're deleted when their block is deleted.
pub struct ViewData {
    /// Render task, running in a thread pool
    pub task: Option<RenderTask>,

    /// Interaction canvas
    pub canvas: ViewCanvas,

    /// Current image
    image: Option<ViewImage>,

    /// Initial render depth, used to render faster
    start_level: usize,

    /// Pending render task, with a new start level
    pending: Option<usize>,

    /// Monotonic counter to identify the most recent task
    generation: u64,
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
            pending: None,
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

/// Set of SDF images, along with their position and metadata
#[derive(Clone)]
pub struct SdfViewImage {
    pub data: Vec<SdfImageData>,
    pub view: fidget::gui::View2,
    pub size: fidget::render::ImageSize,
    pub level: usize,
}

/// Single SDF image to be drawn to the screen
#[derive(Clone)]
pub struct SdfImageData {
    pub distance: Arc<[f32]>,
    pub color: Option<Arc<[[u8; 4]]>>,
}

/// Set of bitfield images, along with their position and metadata
#[derive(Clone)]
pub struct BitfieldViewImage {
    pub data: Vec<BitfieldImageData>,
    pub view: fidget::gui::View2,
    pub size: fidget::render::ImageSize,
    pub level: usize,
}

/// Single bitfield image to be drawn to the screen
#[derive(Clone)]
pub struct BitfieldImageData {
    pub distance: Arc<[f32]>,
    pub color: Option<Arc<[[u8; 4]]>>,
}

impl BitfieldViewImage {
    /// Convert a distance image into a bitfield image, with denoising
    ///
    /// Filled pixels are normally converted to ±∞, but this can cause glitches
    /// if they're on the edge of the model: linear interpolation in the texture
    /// unit means that any pixel touching the infinite pixel will also be
    /// infinite.
    ///
    /// Denoising converts those infinite pixels into the average of their
    /// neighbors, to reduce visual glitches when rendering lower-than-native
    /// resolution images.
    pub fn denoise(
        image: fidget::raster::Image<fidget::raster::DistancePixel>,
        threads: Option<&fidget::render::ThreadPool>,
    ) -> fidget::raster::Image<f32> {
        let mut out = fidget::raster::Image::new(image.size());
        out.apply_effect(
            |x, y| match image[(y, x)].distance() {
                Ok(v) => v,
                Err(f) => {
                    // Replace fill pixels with the average of their
                    // actual-distance neighbors, falling back to infinity if
                    // that fails.  This prevents glitchiness on the edges of
                    // models.  If a fill pixel is exactly at the edge of a
                    // model, linear interpolation in the texture means that
                    // every pixel interpolated with the infinite pixel is also
                    // infinite.
                    let mut inside_count = 0;
                    let mut inside_avg = 0.0;
                    let mut outside_count = 0;
                    let mut outside_avg = 0.0;
                    for dx in [-1, 0, 1] {
                        let Some(x) = x.checked_add_signed(dx) else {
                            continue;
                        };
                        if x >= image.width() {
                            continue;
                        }
                        for dy in [-1, 0, 1] {
                            let Some(y) = y.checked_add_signed(dy) else {
                                continue;
                            };
                            if y >= image.height() {
                                continue;
                            }
                            if let Ok(d) = image[(y, x)].distance() {
                                if d < 0.0 {
                                    inside_avg += d;
                                    inside_count += 1;
                                } else if d > 0.0 {
                                    outside_avg += d;
                                    outside_count += 1;
                                }
                            }
                        }
                    }
                    if f.inside && inside_count > 0 {
                        inside_avg / inside_count as f32
                    } else if !f.inside && outside_count > 0 {
                        outside_avg / outside_count as f32
                    } else if inside_count + outside_count > 0 {
                        (inside_avg + outside_avg)
                            / (inside_count + outside_count) as f32
                    } else if f.inside {
                        -f32::INFINITY
                    } else {
                        f32::INFINITY
                    }
                }
            },
            threads,
        );
        out
    }
}

/// Set of debug images, along with their position and metadata
#[derive(Clone)]
pub struct DebugViewImage {
    pub data: Vec<DebugImageData>,
    pub view: fidget::gui::View2,
    pub size: fidget::render::ImageSize,
    pub level: usize,
}

/// Single debug image to be drawn to the screen
#[derive(Clone)]
pub struct DebugImageData {
    pub pixels: Arc<[[u8; 4]]>,
    // No diffuse color here, this is just a debug view
}

/// Set of heightmap images, along with their position and metadata
#[derive(Clone)]
pub struct HeightmapViewImage {
    pub data: Vec<HeightmapImageData>,
    pub view: fidget::gui::View3,
    pub size: fidget::render::VoxelSize,
    pub level: usize,
}

/// Single heightmap image to be drawn to the screen
#[derive(Clone)]
pub struct HeightmapImageData {
    pub depth: Arc<[f32]>,
    pub color: Option<Arc<[[u8; 4]]>>,
}

/// Set of shaded images, along with their position and metadata
#[derive(Clone)]
pub struct ShadedViewImage {
    pub data: Vec<ShadedImageData>,
    pub ssao: Arc<[f32]>,
    pub view: fidget::gui::View3,
    pub size: fidget::render::VoxelSize,
    pub level: usize,
}

/// Single shaded image to be drawn to the screen
#[derive(Clone)]
pub struct ShadedImageData {
    pub pixels: Arc<[fidget::raster::GeometryPixel]>,
    pub color: Arc<[[u8; 4]]>,
}

/// Rendered image(s) to be drawn, along with the settings that generated it
#[derive(Clone, strum::EnumDiscriminants)]
#[strum_discriminants(name(ViewCanvasType))]
pub enum ViewImage {
    Sdf(SdfViewImage),
    Bitfield(BitfieldViewImage),
    Debug(DebugViewImage),
    Heightmap(HeightmapViewImage),
    Shaded(ShadedViewImage),
}

impl ViewImage {
    pub fn level(&self) -> usize {
        match self {
            ViewImage::Sdf(i) => i.level,
            ViewImage::Bitfield(i) => i.level,
            ViewImage::Debug(i) => i.level,
            ViewImage::Heightmap(i) => i.level,
            ViewImage::Shaded(i) => i.level,
        }
    }
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
            pending: None,
        }
    }

    /// Callback when a render task is complete
    pub fn update(
        &mut self,
        generation: u64,
        data: ViewImage,
        render_time: Duration,
    ) {
        const TARGET_RENDER_TIME: Duration = Duration::from_millis(33);
        const MAX_LEVEL: usize = 10;

        // Adjust self.start_level to hit a render time target
        if data.level() == self.start_level {
            if render_time > TARGET_RENDER_TIME && data.level() < MAX_LEVEL {
                self.start_level += 1;
            } else if render_time < TARGET_RENDER_TIME * 3 / 4 {
                self.start_level = self.start_level.saturating_sub(1);
            }
        }
        if generation == self.generation {
            if let Some(task) = &mut self.task {
                task.set_done();
            }
            if let Some(next) = data.level().checked_sub(1) {
                self.pending = Some(next);
            }
            self.image = Some(data);
        }
    }

    /// Gets the image, kicking off new render jobs if needed
    ///
    /// This should be called in the main GUI loop, or whenever `notify` has
    /// pinged the main loop.
    pub(crate) fn image<N: Notify>(
        &mut self,
        block: BlockIndex,
        scene: Scene,
        tx: &MessageGenSender<N>,
    ) -> Option<&ViewImage> {
        // If the image settings have changed, then clear `task` (which causes
        // us to reinitialize it below).  Skip clearing the task if it's a
        // max-level (i.e. lowest-resolution) render, to preserve responsiveness
        let settings = RenderSettings::from_canvas(&self.canvas, scene);
        if let Some(prev) = &self.task
            && prev.should_cancel(&settings, self.start_level)
        {
            self.task = None;
            self.pending = None;
        }
        if self.task.is_none() {
            self.generation += 1;
            self.task = Some(RenderTask::spawn(
                block,
                self.generation,
                settings,
                self.start_level,
                tx.clone(),
            ));
        } else if let Some(next) = self.pending.take() {
            self.generation += 1;
            self.task = Some(RenderTask::spawn(
                block,
                self.generation,
                settings,
                next,
                tx.clone(),
            ));
        }

        self.image.as_ref()
    }

    pub fn prev_image(&self) -> Option<&ViewImage> {
        self.image.as_ref()
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
            mode: ViewMode2::Debug,
            ..
        } => ViewCanvasType::Debug,
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
            ui.selectable_value(
                &mut tag,
                ViewCanvasType::Debug,
                "2D debug view",
            );
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
            ViewCanvasType::Sdf
            | ViewCanvasType::Bitfield
            | ViewCanvasType::Debug => ViewCanvas::Canvas2 {
                canvas: fidget::gui::Canvas2::new(size),
                mode: match tag {
                    ViewCanvasType::Sdf => ViewMode2::Sdf,
                    ViewCanvasType::Bitfield => ViewMode2::Bitfield,
                    ViewCanvasType::Debug => ViewMode2::Debug,
                    _ => unreachable!(),
                },
            },
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
                        _ => unreachable!(),
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
