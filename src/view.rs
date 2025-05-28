use crate::render::RenderData;

pub struct ViewData {
    pub render: RenderData,
    pub canvas: fidget::gui::Canvas2,
}

impl ViewData {
    pub fn new(image_size: fidget::render::ImageSize) -> Self {
        Self {
            render: RenderData::default(),
            canvas: fidget::gui::Canvas2::new(image_size),
        }
    }
}
