//! Tools for treating Fidget's library of shapes as blocks

use facet::Facet;
use fidget::shapes::{visit_shapes, ShapeVisitor};
use heck::ToSnakeCase;
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
            script: "".to_owned(),
            inputs: HashMap::new(),
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

pub struct ShapeDefinition {
    /// Name of the shape type (typically capitalized)
    pub name: String,

    /// Script to use when building this shape as a block
    pub script: String,

    /// Inputs to populate when building this shape as a block
    pub inputs: HashMap<String, String>,

    /// Category of shape
    ///
    /// The UI adds separator between categories in the selection menu
    pub category: ShapeCategory,
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
        let mut inputs: HashMap<String, String> = HashMap::new();
        for f in s.fields {
            let field_name = f.name;
            let std::collections::hash_map::Entry::Vacant(i) =
                inputs.entry(field_name.to_owned())
            else {
                panic!("duplicate field name {field_name} in {shape_name}")
            };

            i.insert(get_field_string(f));
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
        script += "output(\"out\", out);\nview(out);";
        self.lib.shapes.push(ShapeDefinition {
            name: shape_name.to_owned(),
            script,
            inputs,
            category: ShapeCategory::Fidget,
        });
    }
}

fn get_field_string(f: &facet::Field) -> String {
    // Same set of types as `fidget::rhai::shapes::Type`
    get_field_as::<fidget::shapes::Vec2>(
        f,
        |v| format!("[{}, {}]", v.x, v.y),
        "[0, 0]",
    )
    .or_else(|| {
        get_field_as::<fidget::shapes::Vec3>(
            f,
            |v| format!("[{}, {}, {}]", v.x, v.y, v.z),
            "[0, 0, 0]",
        )
    })
    .or_else(|| {
        get_field_as::<fidget::shapes::Vec4>(
            f,
            |v| format!("[{}, {}, {}, {}]", v.x, v.y, v.z, v.w),
            "[0, 0, 0, 0]",
        )
    })
    .or_else(|| get_field_as::<f64>(f, |v| v.to_string(), "0"))
    .or_else(|| {
        get_field_as::<fidget::context::Tree>(
            f,
            |_| unimplemented!("can't format tree yet"),
            "",
        )
    })
    .or_else(|| {
        get_field_as::<Vec<fidget::context::Tree>>(
            f,
            |_| unimplemented!("can't format Vec<Tree> yet"),
            "[]",
        )
    })
    .expect("unknown field type")
}

fn get_field_as<T: Facet<'static>>(
    field: &facet::Field,
    formatter: fn(T) -> String,
    default: &str,
) -> Option<String> {
    if field.shape().id == T::SHAPE.id {
        Some(if let Some(df) = field.vtable.default_fn {
            let mut v = std::mem::MaybeUninit::<T>::uninit();
            let ptr = facet::PtrUninit::new(&mut v);
            // SAFETY: `df` must be a builder for type `T`
            unsafe { df(ptr) };
            // SAFETY: `v` is initialized by `f`
            let v = unsafe { v.assume_init() };
            formatter(v)
        } else {
            default.to_owned()
        })
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
        let r = &sphere.inputs["radius"];
        assert_eq!(r, "1");
        let center = &sphere.inputs["center"];
        assert_eq!(center, "[0, 0, 0]");
    }

    #[test]
    fn scale_vars() {
        let s = ShapeLibrary::build();
        let scale = s.shapes.iter().find(|s| s.name == "Scale").unwrap();
        assert_eq!(scale.name, "Scale");
        let shape = &scale.inputs["shape"];
        assert_eq!(shape, "");
        let scale = &scale.inputs["scale"];
        assert_eq!(scale, "[1, 1, 1]");
    }
}
