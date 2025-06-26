#[derive(Clone)]
pub struct Drawable {
    /// Tree to draw, as a node in the parent [`Scene`]'s context
    pub tree: fidget::context::Tree,

    /// Optional RGB color associated with this shape
    pub color: Option<[u8; 3]>,
}

#[derive(Clone)]
pub struct Scene {
    pub shapes: Vec<Drawable>,
}

impl From<fidget::context::Tree> for Scene {
    fn from(tree: fidget::context::Tree) -> Self {
        Scene {
            shapes: vec![Drawable { tree, color: None }],
        }
    }
}
