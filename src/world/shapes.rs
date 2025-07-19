//! Tools for treating Fidget's library of shapes as blocks

use facet::Facet;
use fidget::shapes::{
    ShapeVisitor,
    types::{Vec2, Vec3, Vec4},
    visit_shapes,
};
use heck::{ToSnakeCase, ToTitleCase};
use log::warn;
use std::collections::{HashMap, HashSet};

struct Visitor {
    names: HashSet<String>,
    lib: ShapeLibrary,
}

pub struct ShapeLibrary {
    pub shapes: Vec<ShapeDefinition>,
}

impl ShapeLibrary {
    pub fn build() -> Self {
        let mut v = Visitor {
            names: HashSet::new(),
            lib: ShapeLibrary { shapes: vec![] },
        };
        v.names.insert("Script".to_owned());
        v.lib.shapes.push(ShapeDefinition {
            name: "Script".to_owned(),
            kind: ShapeKind::Script {
                script: "".to_owned(),
                inputs: HashMap::new(),
            },
            category: ShapeCategory::Halfspace,
        });
        v.lib.shapes.push(ShapeDefinition {
            name: "Value".to_owned(),
            kind: ShapeKind::Value {
                input: "".to_owned(),
            },
            category: ShapeCategory::Halfspace,
        });
        visit_shapes(&mut v);
        v.lib
    }
}

#[derive(Copy, Clone, PartialEq)]
pub enum ShapeCategory {
    Halfspace,
    Fidget,
}

pub struct ShapeInput {
    pub ty: Option<facet::ConstTypeId>,
    pub text: String,
}

pub struct ShapeDefinition {
    /// Name of the shape type (typically capitalized)
    pub name: String,

    /// Shape kind
    pub kind: ShapeKind,

    /// Category of shape
    ///
    /// The UI adds separator between categories in the selection menu
    pub category: ShapeCategory,
}

pub enum ShapeKind {
    Script {
        /// Script to use when building this shape as a block
        script: String,

        /// Inputs to populate when building this shape as a block
        inputs: HashMap<String, ShapeInput>,
    },
    Value {
        /// Input to populate when building this shape as a block
        input: String,
    },
}

impl ShapeVisitor for Visitor {
    fn visit<
        T: Facet<'static>
            + Clone
            + Send
            + Sync
            + Into<fidget::context::Tree>
            + 'static,
    >(
        &mut self,
    ) {
        let shape_name = T::SHAPE.type_identifier;
        if !self.names.insert(shape_name.to_owned()) {
            panic!("duplicate shape name {shape_name}")
        };

        let facet::Type::User(facet::UserType::Struct(s)) = T::SHAPE.ty else {
            panic!("must be a struct-shaped type");
        };
        let mut script = format!(
            "// auto-generated script for fidget::shapes::{shape_name}\n"
        );
        let mut inputs: HashMap<String, _> = HashMap::new();
        for f in s.fields {
            let field_name = f.name;
            let std::collections::hash_map::Entry::Vacant(i) =
                inputs.entry(field_name.to_owned())
            else {
                panic!("duplicate field name {field_name} in {shape_name}")
            };

            i.insert(get_input_field(f));
            script += "\n";
            for line in f.doc {
                script += &format!("// {line}\n");
            }
            script += &format!("let {field_name} = input(\"{field_name}\");\n");
        }
        let obj = format!(
            "#{{ {} }}",
            inputs
                .keys()
                .map(|k| format!("{k}: {k}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        script +=
            &format!("\nlet out = {}({obj});\n", shape_name.to_snake_case());
        script += "output(\"out\", out);";
        self.lib.shapes.push(ShapeDefinition {
            name: shape_name.to_title_case(),
            kind: ShapeKind::Script { script, inputs },
            category: ShapeCategory::Fidget,
        });
    }
}

/// For a field, get a [`ShapeInput`]
///
/// If the field has a default, then build the default object and use it to
/// generate the string; otherwise fall back to a hard-coded per-type string.
fn get_input_field(f: &facet::Field) -> ShapeInput {
    // Same set of types as `fidget::shapes::Type`
    let s =
        get_field_as::<Vec2>(f, |v| format!("[{}, {}]", v.x, v.y), "[0, 0]")
            .or_else(|| {
                get_field_as::<Vec3>(
                    f,
                    |v| format!("[{}, {}, {}]", v.x, v.y, v.z),
                    "[0, 0, 0]",
                )
            })
            .or_else(|| {
                get_field_as::<Vec4>(
                    f,
                    |v| format!("[{}, {}, {}, {}]", v.x, v.y, v.z, v.w),
                    "[0, 0, 0, 0]",
                )
            })
            .or_else(|| get_field_as::<f64>(f, |v| v.to_string(), "0"))
            .or_else(|| {
                get_field_as::<fidget::context::Tree>(
                    f,
                    |_| {
                        warn!("can't format Tree yet");
                        "".to_owned()
                    },
                    "",
                )
            })
            .or_else(|| {
                get_field_as::<Vec<fidget::context::Tree>>(
                    f,
                    |_| {
                        warn!("can't format Vec<Tree> yet");
                        "".to_owned()
                    },
                    "[]",
                )
            })
            .or_else(|| {
                get_field_as::<fidget::shapes::types::Plane>(
                    f,
                    |d| {
                        println!("{d:?}");
                        warn!("can't format Plane yet");
                        "".to_owned()
                    },
                    "plane(\"yz\")",
                )
            });
    if s.is_none() {
        warn!("unknown field type '{}'", f.shape().type_identifier);
    }
    s.map(|(ty, text)| ShapeInput { ty: Some(ty), text })
        .unwrap_or_else(|| ShapeInput {
            ty: None,
            text: String::new(), // fall back to empty string
        })
}

fn get_field_as<T: Facet<'static>>(
    field: &facet::Field,
    formatter: fn(T) -> String,
    default: &str,
) -> Option<(facet::ConstTypeId, String)> {
    if field.shape().id == T::SHAPE.id {
        Some((
            T::SHAPE.id,
            if let Some(df) = field.vtable.default_fn {
                let mut v = std::mem::MaybeUninit::<T>::uninit();
                let ptr = facet::PtrUninit::new(&mut v);
                // SAFETY: `df` must be a builder for type `T`
                unsafe { df(ptr) };
                // SAFETY: `v` is initialized by `f`
                let v = unsafe { v.assume_init() };
                formatter(v)
            } else {
                default.to_owned()
            },
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn sphere_vars() {
        let s = ShapeLibrary::build();
        let sphere = s.shapes.iter().find(|s| s.name == "Sphere").unwrap();
        assert_eq!(sphere.name, "Sphere");
        let ShapeKind::Script { inputs, .. } = &sphere.kind else {
            panic!()
        };
        let r = &inputs["radius"];
        assert_eq!(r.ty, Some(f64::SHAPE.id));
        assert_eq!(r.text, "1");
        let center = &inputs["center"];
        assert_eq!(center.ty, Some(Vec3::SHAPE.id));
        assert_eq!(center.text, "[0, 0, 0]");
    }

    #[test]
    fn scale_vars() {
        let s = ShapeLibrary::build();
        let scale = s.shapes.iter().find(|s| s.name == "Scale").unwrap();
        assert_eq!(scale.name, "Scale");
        let ShapeKind::Script { inputs, .. } = &scale.kind else {
            panic!()
        };
        let shape = &inputs["shape"];
        assert_eq!(shape.ty, Some(fidget::context::Tree::SHAPE.id));
        assert_eq!(shape.text, "");
        let scale = &inputs["scale"];
        assert_eq!(scale.ty, Some(Vec3::SHAPE.id));
        assert_eq!(scale.text, "[1, 1, 1]");
    }
}
