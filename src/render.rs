/// State for rendering images
#[derive(Default)]
pub enum RenderData {
    /// Nothing here yet
    #[default]
    Empty,
    /// Work has been dispatched to the thread pool
    Working {
        tree: fidget::context::Tree,
        view: fidget::render::View2,
        image_size: fidget::render::ImageSize,

        cancel: fidget::render::CancelToken,
        rx: std::sync::mpsc::Receiver<fidget::render::ColorImage>,
    },
    /// We have generated an image
    Done {
        tree: fidget::context::Tree,
        view: fidget::render::View2,
        image: fidget::render::ColorImage,
    },
}

impl RenderData {
    pub fn spawn<F: FnOnce() + Send + Sync + 'static>(
        tree: fidget::context::Tree,
        image_size: fidget::render::ImageSize,
        view: fidget::render::View2,
        notify: F,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = fidget::render::CancelToken::new();
        let cancel_ = cancel.clone();
        let tree_ = tree.clone();
        rayon::spawn(move || {
            let shape = fidget::vm::VmShape::from(tree_);
            let cfg = fidget::render::ImageRenderConfig {
                image_size,
                cancel: cancel_,
                view,
                ..Default::default()
            };
            if let Some(out) =
                cfg.run::<_, fidget::render::SdfRenderMode>(shape)
            {
                let _ = tx.send(out);
                notify();
            }
        });
        Self::Working {
            tree,
            image_size,
            view,
            cancel,
            rx,
        }
    }

    pub fn check<F: FnOnce() + Send + Sync + 'static>(
        &mut self,
        tree: fidget::context::Tree,
        image_size: fidget::render::ImageSize,
        view: fidget::render::View2,
        notify: F,
    ) {
        if let RenderData::Working { rx, tree, view, .. } = self {
            if let Ok(image) = rx.try_recv() {
                *self = RenderData::Done {
                    image,
                    view: *view,
                    tree: tree.clone(),
                }
            }
        }

        // XXX this is expensive tree deduplication!
        let mut ctx = fidget::Context::new();
        let valid = match self {
            RenderData::Empty => false,
            RenderData::Working {
                tree: working_tree,
                view: working_view,
                image_size: working_size,
                ..
            } => {
                working_size == &image_size
                    && working_view.world_to_model() == view.world_to_model()
                    && ctx.import(working_tree) == ctx.import(&tree)
            }
            RenderData::Done {
                tree: working_tree,
                view: working_view,
                image,
                ..
            } => {
                image.size() == image_size
                    && working_view.world_to_model() == view.world_to_model()
                    && ctx.import(working_tree) == ctx.import(&tree)
            }
        };

        if !valid {
            if let RenderData::Working { cancel, .. } = self {
                cancel.cancel();
            }
            *self = Self::spawn(tree, image_size, view, notify);
        }
    }

    pub fn image(&self) -> Option<&fidget::render::ColorImage> {
        match self {
            RenderData::Empty | RenderData::Working { .. } => None,
            RenderData::Done { image, .. } => Some(image),
        }
    }
}
