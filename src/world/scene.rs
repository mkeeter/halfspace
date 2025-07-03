use fidget::rhai::FromDynamic;

#[derive(Clone)]
pub struct Drawable {
    /// Tree to draw, as a node in the parent [`Scene`]'s context
    pub tree: fidget::context::Tree,

    /// Optional RGB color associated with this shape
    pub color: Option<Color>,
}

impl rhai::CustomType for Drawable {
    fn build(mut builder: rhai::TypeBuilder<Self>) {
        builder
            .with_name("Drawable")
            .on_print(|_t| "drawable(..)".to_owned())
            .with_fn("draw", |tree: fidget::context::Tree, color: Color| {
                Drawable {
                    tree,
                    color: Some(color),
                }
            });
    }
}

#[derive(Clone, PartialEq)]
pub enum Color {
    /// Uniform color
    Rgb([u8; 3]),

    /// Per-pixel evaluation of RGB trees
    RgbPixel([fidget::context::Tree; 3]),
}

impl rhai::CustomType for Color {
    fn build(mut builder: rhai::TypeBuilder<Self>) {
        builder
            .with_name("Color")
            .on_print(|t| match t {
                Color::Rgb([r, g, b]) => format!("rgb({r}, {g}, {b})"),
                Color::RgbPixel(_) => "rgb(..)".to_owned(),
            })
            .with_fn(
                "rgb",
                |ctx: rhai::NativeCallContext,
                 r: rhai::Dynamic,
                 g: rhai::Dynamic,
                 b: rhai::Dynamic|
                 -> Result<Color, Box<rhai::EvalAltResult>> {
                    let mut trees = [r.clone(), g.clone(), b.clone()]
                        .map(|x| x.try_cast::<fidget::context::Tree>());
                    if trees.iter().any(Option::is_some) {
                        let r = match trees[0].take() {
                            Some(t) => t,
                            None => fidget::context::Tree::from_dynamic(
                                &ctx, r, None,
                            )?,
                        };
                        let g = match trees[1].take() {
                            Some(t) => t,
                            None => fidget::context::Tree::from_dynamic(
                                &ctx, g, None,
                            )?,
                        };
                        let b = match trees[2].take() {
                            Some(t) => t,
                            None => fidget::context::Tree::from_dynamic(
                                &ctx, b, None,
                            )?,
                        };
                        Ok(Color::RgbPixel([r, g, b]))
                    } else {
                        let r = f64::from_dynamic(&ctx, r, None)?;
                        let g = f64::from_dynamic(&ctx, g, None)?;
                        let b = f64::from_dynamic(&ctx, b, None)?;
                        let rgb = [r, g, b];
                        if let Some(bad) =
                            rgb.iter().find(|c| !(0.0..=1.0).contains(*c))
                        {
                            return Err(
                                rhai::EvalAltResult::ErrorMismatchDataType(
                                    "float in the 0.0 - 1.0 range".to_owned(),
                                    format!("float with value {bad}"),
                                    ctx.position(),
                                )
                                .into(),
                            );
                        }
                        Ok(Color::Rgb(rgb.map(|x| (x * 255.0) as u8)))
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

impl From<Drawable> for Scene {
    fn from(d: Drawable) -> Self {
        Scene { shapes: vec![d] }
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
                        "tree or drawable".to_owned(),
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
    engine.build_type::<Color>();

    engine.register_fn("*", |tree: fidget::context::Tree, color: Color| {
        Drawable {
            tree,
            color: Some(color),
        }
    });
}
