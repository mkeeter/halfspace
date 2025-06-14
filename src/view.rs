use crate::{
    render::{RenderSettings, RenderTask},
    BlockIndex, Message,
};
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
