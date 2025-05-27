/// State for rendering images
#[derive(Default)]
pub enum RenderData {
    /// Nothing here yet
    #[default]
    Empty,
    /// Work has been dispatched to the thread pool
    Working {
        tree: fidget::context::Tree,
        cancel: fidget::render::CancelToken,
        rx: std::sync::mpsc::Receiver<fidget::render::ColorImage>,
        size: fidget::render::ImageSize,
    },
    /// We have generated an image
    Done {
        tree: fidget::context::Tree,
        image: fidget::render::ColorImage,
    },
}

impl RenderData {
    pub fn spawn<F: FnOnce() + Send + Sync + 'static>(
        tree: fidget::context::Tree,
        image_size: fidget::render::ImageSize,
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
            cancel,
            rx,
            size: image_size,
        }
    }

    pub fn check<F: FnOnce() + Send + Sync + 'static>(
        &mut self,
        tree: fidget::context::Tree,
        image_size: fidget::render::ImageSize,
        notify: F,
    ) {
        if let RenderData::Working { rx, tree, .. } = self {
            if let Ok(image) = rx.try_recv() {
                *self = RenderData::Done {
                    image,
                    tree: tree.clone(),
                }
            }
        }

        let valid = match self {
            RenderData::Empty => false,
            RenderData::Working {
                tree: working_tree,
                size,
                ..
            } => &tree == working_tree && size == &image_size,
            RenderData::Done {
                tree: working_tree,
                image,
                ..
            } => &tree == working_tree && image.size() == image_size,
        };

        if !valid {
            if let RenderData::Working { cancel, .. } = self {
                cancel.cancel();
            }
            *self = Self::spawn(tree, image_size, notify);
        }
    }

    pub fn image(&self) -> Option<&fidget::render::ColorImage> {
        match self {
            RenderData::Empty | RenderData::Working { .. } => None,
            RenderData::Done { image, .. } => Some(image),
        }
    }
}
