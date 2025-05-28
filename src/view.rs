use crate::{BlockIndex, Message};

/// State associated with a given view in the GUI
pub struct ViewData {
    pub task: Option<RenderTask>,
    pub canvas: fidget::gui::Canvas2,
    image: Option<ViewImage>,
    generation: u64,
}

/// Image to be rendered
#[derive(Clone)]
pub struct ViewImage {
    pub data: Vec<[u8; 4]>,
    pub settings: RenderSettings,
}

impl ViewData {
    pub fn new(image_size: fidget::render::ImageSize) -> Self {
        Self {
            task: None,
            canvas: fidget::gui::Canvas2::new(image_size),
            image: None,
            generation: 0,
        }
    }

    pub fn update(
        &mut self,
        generation: u64,
        data: Vec<[u8; 4]>,
        settings: RenderSettings,
    ) {
        if generation == self.generation {
            self.image = Some(ViewImage { data, settings });
        }
    }

    pub fn image(&self) -> Option<&ViewImage> {
        self.image.as_ref()
    }

    pub fn check<F: FnOnce() + Send + Sync + 'static>(
        &mut self,
        block: BlockIndex,
        settings: RenderSettings,
        tx: std::sync::mpsc::Sender<Message>,
        notify: F,
    ) {
        if let Some(prev) = &self.task {
            if prev.settings != settings {
                self.task = None;
            }
        }
        if self.task.is_none() {
            self.generation += 1;
            self.task = Some(RenderTask::spawn(
                block,
                self.generation,
                settings,
                tx,
                notify,
            ));
        }
    }
}

/// State representing an in-progress render
pub struct RenderTask {
    pub settings: RenderSettings,
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
    pub view: fidget::render::View2,
    pub size: fidget::render::ImageSize,
}

impl std::cmp::PartialEq for RenderSettings {
    fn eq(&self, other: &Self) -> bool {
        // XXX this is expensive tree deduplication!
        let mut ctx = fidget::Context::new();
        self.size == other.size
            && self.view.world_to_model() == other.view.world_to_model()
            && ctx.import(&self.tree) == ctx.import(&other.tree)
    }
}

impl RenderTask {
    /// Begins a new image rendering task in the global thread pool
    pub fn spawn<F: FnOnce() + Send + Sync + 'static>(
        block: BlockIndex,
        generation: u64,
        settings: RenderSettings,
        tx: std::sync::mpsc::Sender<Message>,
        notify: F,
    ) -> Self {
        let cancel = fidget::render::CancelToken::new();
        let cancel_ = cancel.clone();
        let settings_ = settings.clone();
        let tree = settings.tree.clone();
        let image_size = settings.size;
        let view = settings.view;
        rayon::spawn(move || {
            let shape = fidget::vm::VmShape::from(tree);
            let cfg = fidget::render::ImageRenderConfig {
                image_size,
                cancel: cancel_,
                view,
                ..Default::default()
            };
            if let Some(image) =
                cfg.run::<_, fidget::render::SdfRenderMode>(shape)
            {
                let (data, _size) =
                    image.map(|&[r, g, b]| [r, g, b, u8::MAX]).take();
                let _ = tx.send(Message::RenderView {
                    block,
                    generation,
                    settings: settings_,
                    data,
                });
                notify();
            }
        });
        Self { settings, cancel }
    }
}
