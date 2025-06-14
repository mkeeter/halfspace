use crate::{BlockIndex, Message};
use serde::{Deserialize, Serialize};

#[cfg(feature = "jit")]
type RenderShape = fidget::jit::JitShape;

#[cfg(not(feature = "jit"))]
type RenderShape = fidget::vm::VmShape;

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

impl From<ViewState> for ViewCanvas {
    fn from(value: ViewState) -> Self {
        match value {
            // Use dummy sizes for the canvas; they'll be updated on the first
            // drawing pass.
            ViewState::View2(mode) => Self::Canvas2 {
                canvas: fidget::gui::Canvas2::new(
                    fidget::render::ImageSize::new(64, 64),
                ),
                mode,
            },
            ViewState::View3(mode) => Self::Canvas3 {
                canvas: fidget::gui::Canvas3::new(
                    fidget::render::VoxelSize::new(64, 64, 64),
                ),
                mode,
            },
        }
    }
}

impl From<ViewCanvas> for ViewState {
    fn from(value: ViewCanvas) -> Self {
        match value {
            ViewCanvas::Canvas2 { mode, .. } => ViewState::View2(mode),
            ViewCanvas::Canvas3 { mode, .. } => ViewState::View3(mode),
        }
    }
}

/// Serializable view state
#[derive(Copy, Clone, Serialize, Deserialize)]
pub enum ViewState {
    View2(ViewMode2),
    View3(ViewMode3),
}

#[derive(Clone, strum::EnumDiscriminants)]
#[strum_discriminants(name(ViewMode2))]
#[strum_discriminants(derive(Serialize, Deserialize))]
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

#[derive(Clone, strum::EnumDiscriminants)]
#[strum_discriminants(name(ViewMode3))]
#[strum_discriminants(derive(Serialize, Deserialize))]
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
pub struct ViewImage {
    pub data: RenderData,
    pub level: usize,
}

impl ViewImage {
    pub fn as_bytes(&self) -> &[u8] {
        self.data.as_bytes()
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
        if data.level == self.start_level {
            if render_time > TARGET_RENDER_TIME && data.level < MAX_LEVEL {
                self.start_level += 1;
            } else if render_time < TARGET_RENDER_TIME * 3 / 4 {
                self.start_level = self.start_level.saturating_sub(1);
            }
        }
        if generation == self.generation {
            if let Some(task) = &mut self.task {
                task.done = true;
            }
            if let Some(next) = data.level.checked_sub(1) {
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
        settings: RenderSettings,
        tx: std::sync::mpsc::Sender<Message>,
        notify: F,
    ) -> Option<&ViewImage> {
        // If the image settings have changed, then clear `task` (which causes
        // us to reinitialize it below).  Only clear the task if it's not a
        // max-level render (to preserve responsiveness)
        if let Some(prev) = &self.task {
            if prev.settings != settings
                && (prev.done || prev.level != self.start_level)
            {
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

/// State representing an in-progress render
pub struct RenderTask {
    settings: RenderSettings,
    level: usize,
    done: bool,
    cancel: fidget::render::CancelToken,
}

impl Drop for RenderTask {
    fn drop(&mut self) {
        self.cancel.cancel()
    }
}

impl RenderTask {
    pub fn done(&self) -> bool {
        self.done
    }
}

/// Settings for rendering an image
#[derive(Clone)]
pub struct RenderSettings {
    pub tree: fidget::context::Tree,
    pub mode: RenderMode,
}

/// Image rendering mode (tied to a canvas, so without a tree)
#[derive(Copy, Clone, PartialEq)]
pub enum RenderMode {
    Render2 {
        mode: ViewMode2,
        view: fidget::render::View2,
        size: fidget::render::ImageSize,
    },
    Render3 {
        mode: ViewMode3,
        view: fidget::render::View3,
        size: fidget::render::VoxelSize,
    },
}

/// Rendered image data
#[derive(Clone)]
pub enum RenderData {
    Render2 {
        data: ViewData2,
        view: fidget::render::View2,
        size: fidget::render::ImageSize,
    },
    Render3 {
        data: ViewData3,
        view: fidget::render::View3,
        size: fidget::render::VoxelSize,
    },
}

impl RenderData {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            RenderData::Render2 { data, .. } => data.as_bytes(),
            RenderData::Render3 { data, .. } => data.as_bytes(),
        }
    }
}

impl std::cmp::PartialEq for RenderSettings {
    fn eq(&self, other: &Self) -> bool {
        // XXX this does expensive tree deduplication!
        let mut ctx = fidget::Context::new();
        self.mode == other.mode
            && ctx.import(&self.tree) == ctx.import(&other.tree)
    }
}

impl RenderTask {
    /// Begins a new image rendering task in the global thread pool
    pub fn spawn<F: FnOnce() + Send + Sync + 'static>(
        block: BlockIndex,
        generation: u64,
        settings: RenderSettings,
        level: usize,
        tx: std::sync::mpsc::Sender<Message>,
        notify: F,
    ) -> Self {
        let cancel = fidget::render::CancelToken::new();
        let cancel_ = cancel.clone();
        let settings_ = settings.clone();
        let start_time = std::time::Instant::now();
        rayon::spawn(move || {
            if let Some(data) = Self::run(&settings_, level, cancel_) {
                if tx
                    .send(Message::RenderView {
                        block,
                        generation,
                        start_time,
                        data,
                    })
                    .is_ok()
                {
                    notify();
                }
            }
        });
        Self {
            settings,
            cancel,
            level,
            done: false,
        }
    }

    pub fn run(
        settings: &RenderSettings,
        level: usize,
        cancel: fidget::render::CancelToken,
    ) -> Option<ViewImage> {
        let scale = 1 << level;
        let data = match settings.mode {
            RenderMode::Render2 { mode, view, size } => {
                let image_size = fidget::render::ImageSize::new(
                    (size.width() / scale).max(1),
                    (size.height() / scale).max(1),
                );
                let cfg = fidget::render::ImageRenderConfig {
                    image_size,
                    view,
                    cancel,
                    pixel_perfect: matches!(mode, ViewMode2::Sdf),
                    ..Default::default()
                };
                let shape = RenderShape::from(settings.tree.clone());
                let tmp = cfg.run(shape)?;
                let data = match mode {
                    ViewMode2::Bitfield => ViewData2::Bitfield(
                        tmp.into_iter()
                            .map(|d| match d.distance() {
                                Ok(d) => d,
                                Err(e) => {
                                    if e.inside {
                                        -f32::INFINITY
                                    } else {
                                        f32::INFINITY
                                    }
                                }
                            })
                            .collect(),
                    ),
                    ViewMode2::Sdf => ViewData2::Sdf(
                        tmp.into_iter()
                            .map(|d| {
                                let d = d.distance().unwrap();
                                if d.is_infinite() {
                                    1e12f32.copysign(d)
                                } else {
                                    d
                                }
                            })
                            .collect(),
                    ),
                };
                RenderData::Render2 { data, view, size }
            }
            RenderMode::Render3 { mode, view, size } => {
                let image_size = fidget::render::VoxelSize::new(
                    (size.width() / scale).max(1),
                    (size.height() / scale).max(1),
                    (size.depth() / scale).max(1),
                );
                let cfg = fidget::render::VoxelRenderConfig {
                    image_size,
                    view,
                    cancel,
                    ..Default::default()
                };
                let shape = RenderShape::from(settings.tree.clone());
                let image = cfg.run(shape)?;
                let data = match mode {
                    ViewMode3::Heightmap => {
                        let max =
                            image.iter().map(|v| v.depth).max().unwrap_or(1);
                        let (data, _size) =
                            image.map(|v| ((v.depth * 255) / max) as u8).take();
                        ViewData3::Heightmap(data)
                    }
                    ViewMode3::Shaded => {
                        // XXX this should all happen on the GPU, probably
                        let threads = Some(&fidget::render::ThreadPool::Global);
                        let image = fidget::render::effects::denoise_normals(
                            &image, threads,
                        );
                        let color = fidget::render::effects::apply_shading(
                            &image, true, threads,
                        );
                        let mut out: fidget::render::Image<[u8; 4], _> =
                            fidget::render::Image::new(image_size);
                        out.apply_effect(
                            |x, y| {
                                let p = image[(y, x)];
                                if p.depth > 0 {
                                    let c = color[(y, x)];
                                    [c[0], c[1], c[2], 255]
                                } else {
                                    [0, 0, 0, 0]
                                }
                            },
                            threads,
                        );
                        let (data, _size) = out.take();
                        ViewData3::Shaded(data)
                    }
                };
                RenderData::Render3 { data, view, size }
            }
        };
        Some(ViewImage { data, level })
    }
}
