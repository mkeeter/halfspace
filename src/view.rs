use crate::{BlockIndex, Message};

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

/// State associated with the canvas (for interactions)
#[derive(strum::EnumDiscriminants)]
#[strum_discriminants(name(ViewCanvasType))]
pub enum ViewCanvas {
    SdfApprox(fidget::gui::Canvas2),
    SdfExact(fidget::gui::Canvas2),
    Bitfield(fidget::gui::Canvas2),
    Heightmap(fidget::gui::Canvas3),
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
            canvas: ViewCanvas::SdfApprox(fidget::gui::Canvas2::new(
                image_size,
            )),
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
        if render_time > TARGET_RENDER_TIME && self.start_level < MAX_LEVEL {
            self.start_level += 1;
        } else if render_time < TARGET_RENDER_TIME * 3 / 4
            && level == self.start_level
        {
            self.start_level = self.start_level.saturating_sub(1);
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

/// Settings for rendering an image
#[derive(Clone)]
pub struct RenderSettings {
    pub tree: fidget::context::Tree,
    pub mode: RenderMode,
}

/// Image rendering mode (tied to a canvas, so without a tree)
#[derive(Copy, Clone, PartialEq)]
pub enum RenderMode {
    SdfApprox(RenderSettings2D),
    SdfExact(RenderSettings2D),
    Bitfield(RenderSettings2D),
    Heightmap(RenderSettings3D),
}

#[derive(Copy, Clone, PartialEq)]
pub struct RenderSettings2D {
    pub view: fidget::render::View2,
    pub size: fidget::render::ImageSize,
}

#[derive(Copy, Clone, PartialEq)]
pub struct RenderSettings3D {
    pub view: fidget::render::View3,
    pub size: fidget::render::VoxelSize,
}

impl std::cmp::PartialEq for RenderSettings {
    fn eq(&self, other: &Self) -> bool {
        // XXX this does expensive tree deduplication!
        let mut ctx = fidget::Context::new();
        let mode_matches = match (self.mode, other.mode) {
            (RenderMode::SdfApprox(a), RenderMode::SdfApprox(b)) => a == b,
            (RenderMode::SdfExact(a), RenderMode::SdfExact(b)) => a == b,
            (RenderMode::Bitfield(a), RenderMode::Bitfield(b)) => a == b,
            _ => false,
        };
        mode_matches && ctx.import(&self.tree) == ctx.import(&other.tree)
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
                let _ = tx.send(Message::RenderView {
                    block,
                    generation,
                    settings: settings_,
                    level,
                    start_time,
                    data,
                });
                notify();
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
            RenderMode::Bitfield(s)
            | RenderMode::SdfApprox(s)
            | RenderMode::SdfExact(s) => {
                let image_size = fidget::render::ImageSize::new(
                    (s.size.width() / scale).max(1),
                    (s.size.height() / scale).max(1),
                );
                let cfg = fidget::render::ImageRenderConfig {
                    image_size,
                    view: s.view,
                    cancel,
                    ..Default::default()
                };
                let shape = fidget::vm::VmShape::from(settings.tree.clone());
                let image = match settings.mode {
                    RenderMode::Bitfield(..) => {
                        let image =
                            cfg.run::<_, fidget::render::BitRenderMode>(shape)?;
                        image.map(|&b| if b { [u8::MAX; 4] } else { [0; 4] })
                    }
                    RenderMode::SdfApprox(..) => {
                        let image =
                            cfg.run::<_, fidget::render::SdfRenderMode>(shape)?;
                        image.map(|&[r, g, b]| [r, g, b, u8::MAX])
                    }
                    RenderMode::SdfExact(..) => {
                        let image = cfg
                            .run::<_, fidget::render::SdfPixelRenderMode>(
                                shape,
                            )?;
                        image.map(|&[r, g, b]| [r, g, b, u8::MAX])
                    }
                    RenderMode::Heightmap(..) => {
                        unreachable!()
                    }
                };
                let (data, _size) = image.take();
                data
            }
            RenderMode::Heightmap(s) => {
                let image_size = fidget::render::VoxelSize::new(
                    (s.size.width() / scale).max(1),
                    (s.size.height() / scale).max(1),
                    (s.size.depth() / scale).max(1),
                );
                let cfg = fidget::render::VoxelRenderConfig {
                    image_size,
                    view: s.view,
                    cancel,
                    ..Default::default()
                };
                let shape = fidget::vm::VmShape::from(settings.tree.clone());
                let image = match settings.mode {
                    RenderMode::Heightmap(..) => {
                        let image = cfg.run(shape)?;
                        image.map(|v| {
                            if v.depth > 0 {
                                let d = (v.depth as usize * 255
                                    / image_size.depth() as usize)
                                    as u8;
                                [d, d, d, 255]
                            } else {
                                [0; 4]
                            }
                        })
                    }
                    RenderMode::SdfExact(_)
                    | RenderMode::SdfApprox(_)
                    | RenderMode::Bitfield(_) => unreachable!(),
                };
                let (data, _size) = image.take();
                data
            }
        };
        Some(data)
    }
}
