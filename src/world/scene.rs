#[derive(Clone)]
pub struct Drawable {
    /// Tree to draw, as a node in the parent [`Scene`]'s context
    pub tree: fidget::context::Tree,

    /// Optional RGB color associated with this shape
    pub color: Option<[u8; 3]>,
}

impl rhai::CustomType for Drawable {
    fn build(mut builder: rhai::TypeBuilder<Self>) {
        builder
            .with_name("Drawable")
            .on_print(|_t| "drawable(..)".to_owned())
            .with_fn(
                "rgb",
                |tree: fidget::context::Tree, r: f64, g: f64, b: f64| {
                    let r = (r.clamp(0.0, 1.0) * u8::MAX as f64) as u8;
                    let g = (g.clamp(0.0, 1.0) * u8::MAX as f64) as u8;
                    let b = (b.clamp(0.0, 1.0) * u8::MAX as f64) as u8;
                    Drawable {
                        tree,
                        color: Some([r, g, b]),
                    }
                },
            );
    }
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

impl rhai::CustomType for Scene {
    fn build(mut builder: rhai::TypeBuilder<Self>) {
        builder
            .with_name("Scene")
            .on_print(|_t| "scene(..)".to_owned())
            .with_fn("scene", build_scene1)
            .with_fn("scene", build_scene2)
            .with_fn("scene", build_scene3)
            .with_fn("scene", build_scene4)
            .with_fn("scene", build_scene5)
            .with_fn("scene", build_scene6)
            .with_fn("scene", build_scene7)
            .with_fn("scene", build_scene8)
            .with_fn("scene", build_scene);
    }
}

macro_rules! scene_builder {
    ($name:ident$(,)? $($v:ident),*) => {
        #[allow(clippy::too_many_arguments)]
        fn $name(
            ctx: rhai::NativeCallContext,
            $($v: rhai::Dynamic),*
        ) -> Result<Scene, Box<rhai::EvalAltResult>> {
            let vs = vec![$( $v ),*];
            build_scene(ctx, vs)
        }
    }
}

fn build_scene(
    ctx: rhai::NativeCallContext,
    vs: Vec<rhai::Dynamic>,
) -> Result<Scene, Box<rhai::EvalAltResult>> {
    let shapes = vs
        .into_iter()
        .map(|v| {
            if let Some(tree) = v.clone().try_cast::<fidget::context::Tree>() {
                Ok(Drawable { tree, color: None })
            } else if let Some(d) = v.clone().try_cast::<Drawable>() {
                Ok(d)
            } else {
                return Err(Box::new(
                    rhai::EvalAltResult::ErrorMismatchDataType(
                        "tree or scene".to_owned(),
                        v.type_name().to_string(),
                        ctx.position(),
                    ),
                ));
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Scene { shapes })
}

scene_builder!(build_scene1, a);
scene_builder!(build_scene2, a, b);
scene_builder!(build_scene3, a, b, c);
scene_builder!(build_scene4, a, b, c, d);
scene_builder!(build_scene5, a, b, c, d, e);
scene_builder!(build_scene6, a, b, c, d, e, f);
scene_builder!(build_scene7, a, b, c, d, e, f, g);
scene_builder!(build_scene8, a, b, c, d, e, f, g, h);

pub fn register_types(engine: &mut rhai::Engine) {
    engine.build_type::<Scene>();
    engine.build_type::<Drawable>();
}
