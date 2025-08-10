use crate::{
    render::{RenderFunction, RenderShape, image_to_bitfield},
    world::Scene,
};

use zerocopy::IntoBytes;

use fidget::{
    context::Tree,
    mesh::{Octree, Settings},
    render::{ImageRenderConfig, ImageSize, RenderHints, ThreadPool},
    shapes::{
        Box, Intersection,
        types::{Vec2, Vec3},
    },
};

#[derive(thiserror::Error, Debug)]
pub enum ExportError {
    #[error("bounds are invalid")]
    InvalidBounds,

    #[error("bounds are too small")]
    BoundsAreTooSmall,

    #[error("min feature {0} is invalid")]
    InvalidMinFeature(f64),

    #[error("min feature is too small")]
    MinFeatureIsTooSmall,

    #[error("export was cancelled")]
    Cancelled,

    #[error("resolution {0} is invalid")]
    InvalidResolution(f64),

    #[error("width {0} is invalid; must be positive")]
    InvalidWidth(f64),

    #[error("height {0} is invalid; must be positive")]
    InvalidHeight(f64),

    #[error("image error")]
    ImageError(#[from] image::ImageError),
}

pub(crate) fn mesh_settings(
    lower: Vec3,
    upper: Vec3,
    feature_size: f64,
) -> Result<fidget::mesh::Settings<'static>, ExportError> {
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
            return Err(ExportError::MinFeatureIsTooSmall);
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
        ..Default::default()
    };
    Ok(settings)
}

/// Returns an exported STL
pub(crate) fn build_stl(
    tree: Tree,
    lower: Vec3,
    upper: Vec3,
    feature_size: f64,
    cancel_token: fidget::render::CancelToken,
) -> Result<Vec<u8>, ExportError> {
    // We intersect the shape with the render bounds, then render with slightly
    // extended bounds (1% larger)
    let bounded: Tree = Intersection {
        input: vec![tree, Box { lower, upper }.into()],
    }
    .into();
    let shape = RenderShape::from(bounded);

    // XXX we do this calculation multiple times: once for the UI, and once
    // again here.  It's cheap, so probably not an issue.
    let mut settings = mesh_settings(lower, upper, feature_size)?;
    settings.cancel = cancel_token;

    let o = Octree::build(&shape, &settings).ok_or(ExportError::Cancelled)?;
    let mesh = o.walk_dual();
    let mut stl = vec![];
    mesh.write_stl(&mut stl).unwrap();
    Ok(stl)
}

fn image_view(
    lower: Vec2,
    upper: Vec2,
    resolution: f64,
) -> Result<fidget::render::View2, ExportError> {
    let center = (lower + upper) / 2.0;
    let scale_xyz = (upper - center).abs().max((lower - center).abs());
    let scale = scale_xyz.x.min(scale_xyz.y);
    if resolution.is_nan() || resolution <= 0.0 {
        return Err(ExportError::InvalidResolution(resolution));
    }

    let center = nalgebra::Vector2::new(center.x as f32, center.y as f32);
    if center.x.is_nan() || center.y.is_nan() {
        return Err(ExportError::InvalidBounds);
    }
    if scale.is_nan() || scale < 1e-8 {
        return Err(ExportError::BoundsAreTooSmall);
    }
    Ok(fidget::render::View2::from_center_and_scale(
        center,
        scale as f32,
    ))
}

pub(crate) fn image_settings(
    lower: Vec2,
    upper: Vec2,
    resolution: f64,
) -> Result<fidget::render::ImageRenderConfig<'static>, ExportError> {
    let view = image_view(lower, upper, resolution)?;

    let size = (upper - lower) * resolution;
    if size.x <= 0.0 {
        return Err(ExportError::InvalidWidth(size.x));
    } else if size.y <= 0.0 {
        return Err(ExportError::InvalidHeight(size.y));
    }
    let width = size.x as u32;
    let height = size.y as u32;

    let settings = ImageRenderConfig {
        image_size: ImageSize::new(width, height),
        world_to_model: view.world_to_model(),
        threads: Some(&ThreadPool::Global),
        tile_sizes: RenderFunction::tile_sizes_2d(),
        ..Default::default()
    };
    Ok(settings)
}

pub(crate) fn build_image(
    scene: Scene,
    lower: Vec2,
    upper: Vec2,
    resolution: f64,
    cancel_token: fidget::render::CancelToken,
) -> Result<Vec<u8>, ExportError> {
    // Some duplicated work here, oh well
    let view = image_view(lower, upper, resolution)?;
    let mut cfg = image_settings(lower, upper, resolution)?;
    cfg.cancel = cancel_token;

    let images: Vec<_> = scene
        .shapes
        .iter()
        .map(|shape| {
            let rs = RenderShape::from(shape.tree.clone());
            let data = cfg.run(rs)?;
            Some(image_to_bitfield(data, view, shape.color.clone()))
        })
        .collect::<Option<_>>()
        .ok_or(ExportError::Cancelled)?;

    let mut out = fidget::render::Image::<[u8; 4]>::new(cfg.image_size);
    out.apply_effect(
        |x, y| {
            let pos = y * cfg.image_size.width() as usize + x;
            for i in images.iter().rev() {
                if i.distance[pos] < 0.0 {
                    let c = i
                        .color
                        .as_ref()
                        .map(|c| c[pos])
                        .unwrap_or([u8::MAX; 4]);
                    if c[3] == 0 {
                        // XXX awkward special cases, otherwise we have
                        // transparent pixels in the utopian example.
                        continue;
                    }
                    return c;
                }
            }
            [0; 4]
        },
        cfg.threads,
    );
    let mut bytes = vec![];
    image::write_buffer_with_format(
        &mut std::io::Cursor::new(&mut bytes),
        out.take().0.as_bytes(),
        cfg.image_size.width(),
        cfg.image_size.height(),
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )?;

    Ok(bytes)
}
