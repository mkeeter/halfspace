use crate::render::RenderShape;

use fidget::{
    context::Tree,
    mesh::{Octree, Settings},
    render::{CancelToken, ThreadPool},
    shapes::{Box, Intersection, types::Vec3},
};

#[derive(thiserror::Error, Debug)]
pub enum ExportError {
    #[error("bounds are invalid")]
    InvalidBounds,

    #[error("bounds are too small")]
    BoundsAreTooSmall,

    #[error("min feature {0} is invalid")]
    InvalidMinFeature(f64),

    #[error("depth is absurd")]
    AbsurdDepth,

    #[error("export was cancelled")]
    Cancelled,
}

/// Returns an exported STL
pub(crate) fn build_stl(
    tree: Tree,
    lower: Vec3,
    upper: Vec3,
    feature_size: f64,
    cancel: CancelToken,
) -> Result<Vec<u8>, ExportError> {
    // We intersect the shape with the render bounds, then render with slightly
    // extended bounds (1% larger)
    let bounded: Tree = Intersection {
        input: vec![tree, Box { lower, upper }.into()],
    }
    .into();
    let shape = RenderShape::from(bounded);
    let center = (lower + upper) / 2.0;
    let scale_xyz = (upper - center).abs().max((lower - center).abs());
    let scale = scale_xyz.x.max(scale_xyz.y).max(scale_xyz.z) * 1.01;
    if feature_size.is_nan() {
        return Err(ExportError::InvalidMinFeature(feature_size));
    }
    let mut depth = 0u8;
    while scale * 2.0 / 2f64.powi(i32::from(depth)) >= feature_size {
        depth += 1;
        if depth >= 20 {
            return Err(ExportError::AbsurdDepth);
        }
    }

    let center = nalgebra::Vector3::new(
        center.x as f32,
        center.y as f32,
        center.z as f32,
    );
    if center.x.is_nan() || center.y.is_nan() || center.z.is_nan() {
        return Err(ExportError::InvalidBounds);
    }
    if scale.is_nan() || scale < 1e-8 {
        return Err(ExportError::BoundsAreTooSmall);
    }

    let view =
        fidget::render::View3::from_center_and_scale(center, scale as f32);
    let settings = Settings {
        depth,
        world_to_model: view.world_to_model(),
        threads: Some(&ThreadPool::Global),
        cancel,
    };
    let o = Octree::build(&shape, &settings).ok_or(ExportError::Cancelled)?;
    let mesh = o.walk_dual();
    let mut stl = vec![];
    mesh.write_stl(&mut stl).unwrap();
    Ok(stl)
}
