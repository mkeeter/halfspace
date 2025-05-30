use crate::{BlockIndex, Message};

/// State associated with a given view in the GUI
pub struct ViewData {
    pub task: Option<RenderTask>,
    pub canvas: ViewCanvas,
    image: Option<ViewImage>,
    generation: u64,
}

/// State associated with the canvas
#[derive(strum::EnumDiscriminants)]
#[strum_discriminants(name(ViewCanvasType))]
pub enum ViewCanvas {
    SdfApprox(fidget::gui::Canvas2),
    SdfExact(fidget::gui::Canvas2),
    Bitfield(fidget::gui::Canvas2),
}

/// Rendered image, along with the settings that generated it
#[derive(Clone)]
pub struct ViewImage {
    pub data: Vec<[u8; 4]>,
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
    pub mode: RenderMode,
}

/// Image rendering mode (tied to a canvas, so without a tree)
#[derive(Copy, Clone, PartialEq)]
pub enum RenderMode {
    SdfApprox(RenderSettings2D),
    SdfExact(RenderSettings2D),
    Bitfield(RenderSettings2D),
}

#[derive(Copy, Clone, PartialEq)]
pub struct RenderSettings2D {
    pub view: fidget::render::View2,
    pub size: fidget::render::ImageSize,
}

impl std::cmp::PartialEq for RenderSettings {
    fn eq(&self, other: &Self) -> bool {
        // XXX this is expensive tree deduplication!
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
        tx: std::sync::mpsc::Sender<Message>,
        notify: F,
    ) -> Self {
        let cancel = fidget::render::CancelToken::new();
        let cancel_ = cancel.clone();
        let settings_ = settings.clone();
        rayon::spawn(move || {
            if let Some(data) = Self::run(&settings_, cancel_) {
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

    pub fn run(
        settings: &RenderSettings,
        cancel: fidget::render::CancelToken,
    ) -> Option<Vec<[u8; 4]>> {
        let data = match settings.mode {
            RenderMode::Bitfield(s)
            | RenderMode::SdfApprox(s)
            | RenderMode::SdfExact(s) => {
                let cfg = fidget::render::ImageRenderConfig {
                    image_size: s.size,
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
                };
                let (data, _size) = image.take();
                data
            }
        };
        Some(data)
    }
}
