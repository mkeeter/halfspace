use crate::{BlockIndex, Message};

/// State for rendering images
pub struct RenderTask {
    pub settings: RenderSettings,
    cancel: fidget::render::CancelToken,
}

impl Drop for RenderTask {
    fn drop(&mut self) {
        self.cancel.cancel()
    }
}

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
