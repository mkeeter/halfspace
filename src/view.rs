use crate::{BlockIndex, Message};
use serde::{Deserialize, Serialize};

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

#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub enum ViewMode2 {
    SdfApprox,
    SdfExact,
    Bitfield,
}

#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub enum ViewMode3 {
    Heightmap,
    Shaded,
}

/// Rendered image, along with the settings that generated it
#[derive(Clone)]
pub struct ViewImage {
    pub data: Vec<[u8; 4]>,
    pub level: usize,
    pub settings: RenderSettings,
}

impl ViewData {
    pub fn new(image_size: fidget::render::ImageSize) -> Self {
        Self {
            task: None,
            canvas: ViewCanvas::Canvas2 {
                canvas: fidget::gui::Canvas2::new(image_size),
                mode: ViewMode2::SdfApprox,
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
        level: usize,
        data: Vec<[u8; 4]>,
        settings: RenderSettings,
        render_time: std::time::Duration,
    ) {
        const TARGET_RENDER_TIME: std::time::Duration =
            std::time::Duration::from_millis(33);
        const MAX_LEVEL: usize = 10;

        // Adjust self.start_level to hit a render time target
        if level == self.start_level {
            if render_time > TARGET_RENDER_TIME && level < MAX_LEVEL {
                self.start_level += 1;
            } else if render_time < TARGET_RENDER_TIME * 3 / 4 {
                self.start_level = self.start_level.saturating_sub(1);
            }
        }
        if generation == self.generation {
            self.image = Some(ViewImage {
                data,
                settings,
                level,
            });
            if let Some(task) = &mut self.task {
                task.done = true;
            }
            if let Some(next) = level.checked_sub(1) {
                self.pending = Some(next);
            }
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
                        settings: settings_,
                        level,
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
    ) -> Option<Vec<[u8; 4]>> {
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
                    ..Default::default()
                };
                let shape = fidget::vm::VmShape::from(settings.tree.clone());
                let image = match mode {
                    ViewMode2::Bitfield => {
                        let image =
                            cfg.run::<_, fidget::render::BitRenderMode>(shape)?;
                        image.map(|&b| if b { [u8::MAX; 4] } else { [0; 4] })
                    }
                    ViewMode2::SdfApprox => {
                        let image =
                            cfg.run::<_, fidget::render::SdfRenderMode>(shape)?;
                        image.map(|&[r, g, b]| [r, g, b, u8::MAX])
                    }
                    ViewMode2::SdfExact => {
                        let image = cfg
                            .run::<_, fidget::render::SdfPixelRenderMode>(
                                shape,
                            )?;
                        image.map(|&[r, g, b]| [r, g, b, u8::MAX])
                    }
                };
                let (data, _size) = image.take();
                data
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
                let shape = fidget::vm::VmShape::from(settings.tree.clone());
                let image = cfg.run(shape)?;
                let image = match mode {
                    ViewMode3::Heightmap => image.map(|v| {
                        if v.depth > 0 {
                            let d = (v.depth as usize * 255
                                / image_size.depth() as usize)
                                as u8;
                            [d, d, d, 255]
                        } else {
                            [0; 4]
                        }
                    }),
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
                        out
                    }
                };
                let (data, _size) = image.take();
                data
            }
        };
        Some(data)
    }
}
