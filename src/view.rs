use crate::{
    render::{RenderSettings, RenderTask},
    BlockIndex, Message,
};

pub struct ViewData {
    pub task: Option<RenderTask>,
    pub canvas: fidget::gui::Canvas2,
    image: Option<ViewImage>,
    generation: u64,
}

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
