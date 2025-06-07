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

            // Same set of types as `fidget::rhai::shapes::Type`
            let t = if f.shape().id == fidget::shapes::Vec2::SHAPE.id {
                if field_name == "upper" || field_name == "scale" {
                    "[1, 1]"
                } else {
                    "[0, 0]"
                }
            } else if f.shape().id == fidget::shapes::Vec3::SHAPE.id {
                if field_name == "upper" || field_name == "scale" {
                    "[1, 1, 1]"
                } else {
                    "[0, 0, 0]"
                }
            } else if f.shape().id == fidget::shapes::Vec4::SHAPE.id {
                "[0, 0, 0, 0]"
            } else if f.shape().id == f64::SHAPE.id {
                if field_name == "radius" || field_name == "scale" {
                    "1"
                } else {
                    "0"
                }
            } else if f.shape().id == fidget::context::Tree::SHAPE.id {
                "x"
            } else if f.shape().id == Vec::<fidget::context::Tree>::SHAPE.id {
                "[x, y]"
            } else {
                panic!("unknown type ID for {}", f.shape().type_identifier)
            };
            i.insert(t.to_owned());
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
