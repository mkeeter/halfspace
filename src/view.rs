use crate::{
    gui::{CAMERA, WARN},
    render::{RenderSettings, RenderTask},
    state,
    state::ViewState,
    BlockIndex, Message, ViewResponse,
};

pub use state::{ViewMode2, ViewMode3};

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
            ViewCanvas::Canvas3 { canvas, mode } => {
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
                    fidget::render::View2::from_components(center, scale),
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
            } => {
                let canvas = fidget::gui::Canvas3::from_components(
                    fidget::render::View3::from_components(
                        center, scale, yaw, pitch,
                    ),
                    fidget::render::VoxelSize::new(width, height, depth),
                );
                Self::Canvas3 { canvas, mode }
            }
        }
    }
}

#[derive(Clone)]
pub enum ViewData2 {
    Sdf(Vec<f32>),
    Bitfield(Vec<f32>),
}

impl ViewData2 {
    pub fn as_bytes(&self) -> &[u8] {
        use zerocopy::IntoBytes;
        match self {
            ViewData2::Sdf(data) => data.as_bytes(),
            ViewData2::Bitfield(data) => data.as_bytes(),
        }
    }
}

#[derive(Clone)]
pub enum ViewData3 {
    /// Normalized heightmap values, with 0 indicating an empty position
    Heightmap(Vec<u8>),
    Shaded(Vec<[u8; 4]>),
}

impl ViewData3 {
    pub fn as_bytes(&self) -> &[u8] {
        use zerocopy::IntoBytes;
        match self {
            ViewData3::Heightmap(data) => data.as_bytes(),
            ViewData3::Shaded(data) => data.as_bytes(),
        }
    }
}

/// Rendered image, along with the settings that generated it
#[derive(Clone)]
pub enum ViewImage {
    View2 {
        data: ViewData2,
        view: fidget::render::View2,
        size: fidget::render::ImageSize,
        level: usize,
    },
    View3 {
        data: ViewData3,
        view: fidget::render::View3,
        size: fidget::render::VoxelSize,
        level: usize,
    },
}

impl ViewImage {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            ViewImage::View2 { data, .. } => data.as_bytes(),
            ViewImage::View3 { data, .. } => data.as_bytes(),
        }
    }

    pub fn level(&self) -> usize {
        match self {
            ViewImage::View2 { level, .. } | ViewImage::View3 { level, .. } => {
                *level
            }
        }
    }
}

impl ViewData {
    pub fn new(image_size: fidget::render::ImageSize) -> Self {
        Self {
            task: None,
            canvas: ViewCanvas::Canvas2 {
                canvas: fidget::gui::Canvas2::new(image_size),
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
        render_time: std::time::Duration,
    ) {
        const TARGET_RENDER_TIME: std::time::Duration =
            std::time::Duration::from_millis(33);
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
    pub fn image<F: FnOnce() + Send + Sync + 'static>(
        &mut self,
        block: BlockIndex,
        tree: fidget::context::Tree,
        tx: std::sync::mpsc::Sender<Message>,
        notify: F,
    ) -> Option<&ViewImage> {
        // If the image settings have changed, then clear `task` (which causes
        // us to reinitialize it below).  Only clear the task if it's not a
        // max-level render (to preserve responsiveness)
        let settings = RenderSettings::from_canvas(&self.canvas, tree);
        if let Some(prev) = &self.task {
            if prev.should_cancel(&settings, self.start_level) {
                self.task = None;
                self.pending = None;
            }
        }
        if self.task.is_none() {
            self.generation += 1;
            self.task = Some(RenderTask::spawn(
                block,
                self.generation,
                settings,
                self.start_level,
                tx,
                notify,
            ));
        } else if let Some(next) = self.pending.take() {
            self.generation += 1;
            self.task = Some(RenderTask::spawn(
                block,
                self.generation,
                settings,
                next,
                tx,
                notify,
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
    #[derive(Copy, Clone, PartialEq, Eq)]
    enum ViewCanvasType {
        Sdf,
        Bitfield,
        Heightmap,
        Shaded,
    }
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
                        _ => unreachable!(),
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
                        _ => unreachable!(),
                    },
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
                    ..
                },
                ViewCanvas::Canvas3 {
                    canvas: prev_canvas,
                    ..
                },
            ) => std::mem::swap(next_canvas, prev_canvas),
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
