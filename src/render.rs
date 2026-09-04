//! Image rendering
//!
//! # Big Theory Statement
//! Each block in the GUI may have 0 or 1 views (represented by a
//! [`ViewData`](crate::view::ViewData)).  The [`App`](crate::App) stores a map
//! from `BlockIndex` to `ViewData`.
//!
//! When updating the UI, we construct a [`WorldView`](crate::gui::WorldView),
//! which implements the [`egui_dock::TabViewer`] trait.  When a view is drawn,
//! we call [`WorldView::view_ui`](crate::gui::WorldView::view_ui), which grabs
//! the `ViewData` for that block.  This in turn calls
//! [`ViewData::image`](crate::view::ViewData::image) to get a [`ViewImage`] to
//! draw.
//!
//! From here, our dive goes in two directions.
//!
//! ## Rendering images
//! [`ViewData::image`](crate::view::ViewData::image) checks to see whether our
//! current settings match those of an in-progress render.  If not, then it
//! cancels the in-progress render and starts a new render, spawning it into the
//! global `rayon` thread pool.  If available, it returns a cached image, which
//! is a [`ViewImage`].
//!
//! A render task is represented by a [`RenderTask`] object, which performs the
//! render then sends a generation-tagged result into a [`MessageGenSender`].
//! Note that there are **two** generations: a global generation associated with
//! the `App`, and a local generation associated with the `ViewData`.  The
//! global generation invalidates messages associated with a previous file; the
//! local generation invalidates render results which arrive out of order (only
//! the newest render task has the correct local generation number).
//!
//! Eventually, the [`RenderTask`] finishes.  It sends [`Message::RenderView`]
//! into the global event queue; the main loop receives it and dispatches to the
//! appropriate [`ViewData::update`](crate::view::ViewData::update).
//!
//! In [`ViewData::update`](crate::view::ViewData::update), the new image data
//! is stored and we adjust the `start_level` based on render time; this is used
//! in subsequent renders to maintain a high frame rate.
//!
//! At the end of this process, we have a [`ViewImage`], which contains pixels
//! in RAM for a particular image type and render settings (angle, image size,
//! etc).  We store this image (and the settings used to generate it) into the
//! `ViewData`, for use in the next check.
//!
//! ## Drawing images to the screen
//! TODO write this
use crate::{
    BlockIndex, Message, MessageGenSender, RenderViewReply,
    platform::Notify,
    view::{
        PixelImage, RgbaImage, ViewCanvas, ViewImage, ViewMode2, ViewMode3,
    },
    world::{Color, Scene},
};

use fidget::{
    eval::{BulkEvaluator, Function, MathFunction},
    raster::{effects, pixel::RawDistancePixel, voxel::GeometryPixel},
};

use rayon::prelude::*;
use std::sync::Arc;
use web_time::Instant;

#[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
pub(crate) type RenderFunction = fidget::jit::JitFunction;

#[cfg(any(target_arch = "wasm32", not(feature = "jit")))]
pub(crate) type RenderFunction = fidget::vm::VmFunction;

pub(crate) type RenderShape = fidget::shape::Shape<RenderFunction>;

/// State representing an in-progress render
///
/// This lives in the main thread; the work itself lives in a closure in the
/// `rayon` or WGPU thread pool.
pub struct RenderTaskHandle {
    kind: RenderTaskKind,
    level: usize,
    cancel: fidget::render::CancelToken,
}

impl Drop for RenderTaskHandle {
    fn drop(&mut self) {
        self.cancel.cancel()
    }
}

pub enum RenderTaskKind {
    Cpu { settings: RenderSettings },
}

/// CPU worker pool, which dispatches to the (global) Rayon thread pool
pub(crate) struct CpuWorkerPool<N: Notify> {
    // TODO actually make an explicit Rayon pool here?
    _marker: std::marker::PhantomData<N>,
}

impl<N: Notify> CpuWorkerPool<N> {
    pub(crate) fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }

    /// Begins a new image rendering task in the global rayon thread pool
    pub(crate) fn spawn(
        &self,
        block: BlockIndex,
        generation: u64,
        settings: RenderSettings,
        level: usize,
        tx: MessageGenSender<N>,
    ) -> RenderTaskHandle {
        let cancel = fidget::render::CancelToken::new();
        let cancel_ = cancel.clone();
        let settings_ = settings.clone();
        let start_time = Instant::now();
        rayon::spawn(move || {
            if let Some(data) = CpuRenderTask::run(&settings_, level, cancel_) {
                tx.send(Message::RenderView(RenderViewReply {
                    block,
                    generation,
                    start_time,
                    data,
                    settings: settings_,
                }))
            }
        });
        RenderTaskHandle {
            kind: RenderTaskKind::Cpu { settings },
            cancel,
            level,
        }
    }
}

impl RenderTaskHandle {
    /// Checks whether the new settings are different from our settings
    ///
    /// This only returns `true` if `self.level != max_level`; we want to avoid
    /// interrupting max-level renders to preserve responsiveness.
    pub fn should_cancel(
        &self,
        other: &RenderSettings,
        max_level: usize,
    ) -> bool {
        let settings_changed = match &self.kind {
            RenderTaskKind::Cpu { settings, .. } => settings != other,
        };
        settings_changed && self.level != max_level
    }
}

/// Dummy object representing a CPU render task
struct CpuRenderTask;

impl CpuRenderTask {
    /// Function which actually renders images (off-thread)
    pub fn run(
        settings: &RenderSettings,
        level: usize,
        cancel: fidget::render::CancelToken,
    ) -> Option<ViewImage> {
        let scale = 1 << level;
        let data = match settings {
            RenderSettings::Image(ImageRenderSettings {
                scene,
                mode,
                view,
                size,
            }) => {
                let threads = Some(&fidget::render::ThreadPool::Global);
                let image_size = fidget::render::ImageSize::new(
                    (size.width() / scale).max(1),
                    (size.height() / scale).max(1),
                );
                let render_cfg = fidget::raster::pixel::RenderConfig {
                    image_size,
                    world_to_model: view.world_to_model(),
                    pixel_perfect: matches!(mode, ViewMode2::Sdf),
                    z: 0.0,
                };

                let eval_cfg = fidget::raster::pixel::EvalConfig {
                    cancel,
                    ..Default::default()
                };
                let images: Vec<_> = scene
                    .shapes
                    .iter()
                    .map(|shape| {
                        let rs = RenderShape::from(shape.tree.clone());
                        let data = fidget::raster::pixel::render(
                            rs.try_into().expect("no vars allowed"),
                            &render_cfg,
                            &eval_cfg,
                        )?;
                        Some((data, shape.color.clone()))
                    })
                    .collect::<Option<_>>()?;
                let (merged, color) =
                    merge_and_color(image_size, *view, images);

                // Denoising replaces NaN values with floats.  This is innocuous
                // for an SDF render (which are already pixel-perfect), but is
                // helpful for deglitching bitfield rendering when rendered at
                // non-native resolution.
                let distance = denoise_2d(merged, threads).take().0.into();

                let image = PixelImage {
                    view: *view,
                    size: *size,
                    level,
                    distance,
                    color: color.map(|c| c.take().0.into()),
                };
                ViewImage::Pixel { image, mode: *mode }
            }
            RenderSettings::Voxel(VoxelRenderSettings {
                scene,
                mode,
                view,
                size,
                perspective,
            }) => {
                // If this is our final rendering level, then do oversampling in
                // the Z direction for better rendering of edges.  XXX if you
                // change this, then you also need to edit `shaded.rs` to adjust
                // the `max_depth` passed into the shader.
                let bonus_z = if level == 0 { 2 } else { 1 };
                let image_size = fidget::render::VoxelSize::new(
                    (size.width() / scale).max(1),
                    (size.height() / scale).max(1),
                    (size.depth() / scale).max(1) * bonus_z,
                );
                let z_scale = 2.0 / bonus_z as f32;
                let scale = nalgebra::Scale3::new(1.0, 1.0, z_scale);
                let mut world_to_model =
                    view.world_to_model() * scale.to_homogeneous();
                if *perspective {
                    *world_to_model.get_mut((3, 2)).unwrap() =
                        0.3 / bonus_z as f32;
                }
                let render_cfg = fidget::raster::voxel::RenderConfig {
                    image_size,
                    world_to_model,
                };
                let eval_cfg = fidget::raster::voxel::EvalConfig {
                    cancel,
                    ..Default::default()
                };
                let images: Vec<_> = scene
                    .shapes
                    .par_iter()
                    .map(|shape| {
                        let rs = RenderShape::from(shape.tree.clone());
                        let data = fidget::raster::voxel::render(
                            rs.try_into().expect("no vars allowed"),
                            &render_cfg,
                            &eval_cfg,
                        )?;
                        let data = data.map(|p| GeometryPixel {
                            depth: p.depth,
                            normal: [
                                p.normal[0],
                                p.normal[1],
                                p.normal[2] / z_scale,
                            ],
                        });
                        Some((data, shape.color.clone()))
                    })
                    .collect::<Option<_>>()?;
                // Merge into a single `(GeometryPixel, shape index)` image for
                // additional post-processing
                let mut merged = TaggedGeometryPixelImage::new(image_size);
                merged.apply_effect(
                    |x, y| {
                        let Some(mut p) =
                            images.first().map(|(img, _c)| img[(y, x)])
                        else {
                            return Default::default();
                        };
                        let mut shape_index = 0;
                        for (i, (img, _color)) in
                            images.iter().enumerate().skip(1)
                        {
                            let q = img[(y, x)];
                            if p.depth < q.depth {
                                p = q;
                                shape_index = i;
                            }
                        }
                        (p, shape_index)
                    },
                    Some(&fidget::render::ThreadPool::Global),
                );
                let colors =
                    images.into_iter().map(|(_img, c)| c).collect::<Vec<_>>();

                let color = match mode {
                    ViewMode3::Heightmap => {
                        image_to_heightmap(merged, *view, colors)
                    }
                    ViewMode3::Shaded => image_to_shaded(merged, *view, colors),
                };
                let image = RgbaImage {
                    view: *view,
                    size: *size,
                    level,
                    color,
                };
                ViewImage::Voxel { mode: *mode, image }
            }
        };
        Some(data)
    }
}

/// Compares two distance pixels
///
/// Returns `true` if we should swap (i.e. replace `a` with `b`)
fn compare_distance_pixel(a: RawDistancePixel, b: RawDistancePixel) -> bool {
    match (a.inside(), b.inside()) {
        (true, false) => false,
        (false, true) => true,
        (true, true) | (false, false) => false,
    }
}

pub(crate) fn merge_and_color(
    image_size: fidget::render::ImageSize,
    view: fidget::gui::View2,
    images: Vec<(fidget::raster::pixel::Image, Option<Color>)>,
) -> (TaggedDistancePixelImage, Option<fidget::raster::RgbaImage>) {
    let mut merged = TaggedDistancePixelImage::new(image_size);
    merged.apply_effect(
        |x, y| {
            let Some(mut p) = images.first().map(|(img, _c)| img[(y, x)])
            else {
                return Default::default();
            };
            let mut shape_index = 0;
            // TODO(fidget) add this merge to `effects`?
            for (i, (img, _color)) in images.iter().enumerate().skip(1) {
                let q = img[(y, x)];
                if compare_distance_pixel(p, q) {
                    p = q;
                    shape_index = i;
                }
            }
            (p, shape_index)
        },
        Some(&fidget::render::ThreadPool::Global),
    );

    let colors = images.into_iter().map(|(_img, c)| c).collect::<Vec<_>>();
    let color = if colors.iter().any(|c| c.is_some()) {
        let mut color = fidget::raster::Image::new(merged.size());
        for y in 0..merged.size().height() {
            for x in 0..merged.size().width() {
                color[(y as usize, x as usize)] = [0xFF; 4];
            }
        }
        for (i, c) in colors.iter().enumerate() {
            let Some(c) = c else {
                continue;
            };
            render_colors_2d(&merged, i, view, c, &mut color);
        }
        Some(color)
    } else {
        None
    };
    (merged, color)
}

type TaggedGeometryPixelImage =
    fidget::raster::Image<(GeometryPixel, usize), fidget::render::VoxelSize>;
type TaggedDistancePixelImage =
    fidget::raster::Image<(RawDistancePixel, usize), fidget::render::ImageSize>;

/// Settings for rendering an image
#[derive(Clone, PartialEq)]
pub enum RenderSettings {
    Image(ImageRenderSettings),
    Voxel(VoxelRenderSettings),
}

#[derive(Clone, PartialEq)]
pub struct ImageRenderSettings {
    scene: Scene,
    mode: ViewMode2,
    view: fidget::gui::View2,
    size: fidget::render::ImageSize,
}

#[derive(Clone, PartialEq)]
pub struct VoxelRenderSettings {
    scene: Scene,
    mode: ViewMode3,
    perspective: bool,
    view: fidget::gui::View3,
    size: fidget::render::VoxelSize,
}

impl RenderSettings {
    pub fn from_canvas(canvas: &ViewCanvas, scene: Scene) -> Self {
        match canvas {
            ViewCanvas::Canvas2 { canvas, mode } => {
                RenderSettings::Image(ImageRenderSettings {
                    scene,
                    view: canvas.view(),
                    size: canvas.image_size(),
                    mode: *mode,
                })
            }
            ViewCanvas::Canvas3 {
                canvas,
                mode,
                perspective,
            } => {
                let size = canvas.image_size();
                RenderSettings::Voxel(VoxelRenderSettings {
                    scene,
                    view: canvas.view(),
                    perspective: *perspective,
                    size: fidget::render::VoxelSize::new(
                        size.width(),
                        size.height(),
                        // XXX select depth?
                        size.width().max(size.height()),
                    ),
                    mode: *mode,
                })
            }
        }
    }
}

fn image_to_heightmap(
    image: TaggedGeometryPixelImage,
    view: fidget::gui::View3,
    colors: Vec<Option<Color>>,
) -> Arc<[[u8; 4]]> {
    let threads = Some(&fidget::render::ThreadPool::Global);

    // Build an accumulated color image, starting with all white
    // TODO(fidget) better way of doing this?
    let mut color = fidget::raster::Image::new(image.size());
    let mut max_depth = 1;
    let mut min_depth = u32::MAX;
    for y in 0..image.size().height() {
        for x in 0..image.size().width() {
            color[(y as usize, x as usize)] = [0xFF; 4];
            let d = image[(y as usize, x as usize)].0.depth;
            max_depth = max_depth.max(d);
            if d != 0 {
                min_depth = min_depth.min(d);
            }
        }
    }
    for (i, c) in colors.iter().enumerate() {
        let Some(c) = c else {
            continue;
        };
        render_colors_3d(&image, i, view, c, &mut color);
    }

    // Strip shape index
    let image = image.map(|f| f.0);

    // Apply brightness based on depth
    let mut out = fidget::raster::Image::new(image.size());
    out.apply_effect(
        |x, y| {
            if image[(y, x)].depth == 0 {
                [0; 4]
            } else {
                // Scale based on height, but not all the way to black
                let brightness = (image[(y, x)].depth as f32
                    - min_depth as f32)
                    / (max_depth - min_depth) as f32
                    * 0.7
                    + 0.3;
                color[(y, x)].map(|i| (i as f32 * brightness) as u8)
            }
        },
        threads,
    );
    out.take().0.into()
}

fn image_to_shaded(
    image: TaggedGeometryPixelImage,
    view: fidget::gui::View3,
    colors: Vec<Option<Color>>,
) -> Arc<[[u8; 4]]> {
    let threads = Some(&fidget::render::ThreadPool::Global);

    // Build an accumulated color image, starting with all white
    // TODO(fidget) better way of doing this?
    let mut color = fidget::raster::Image::new(image.size());
    for y in 0..image.size().height() {
        for x in 0..image.size().width() {
            color[(y as usize, x as usize)] = [0xFF; 4];
        }
    }
    for (i, c) in colors.iter().enumerate() {
        let Some(c) = c else {
            continue;
        };
        render_colors_3d(&image, i, view, c, &mut color);
    }

    // Strip shape index
    let image = image.map(|f| f.0);
    let image = effects::denoise_normals(&image, threads);

    let shaded = effects::apply_shading(&image, true, threads);
    let mut out = fidget::raster::Image::new(image.size());
    out.apply_effect(
        |x, y| {
            if image[(y, x)].depth == 0 {
                [0; 4]
            } else {
                // TODO(fidget) intensity is the same for all channels
                let brightness = shaded[(y, x)][0] as u16;
                color[(y, x)].map(|i| ((i as u16 * brightness) >> 8) as u8)
            }
        },
        threads,
    );
    out.take().0.into()
}

pub(crate) fn hsl_to_rgb(hsl: [u8; 4]) -> [u8; 4] {
    use palette::{FromColor, Hsl, Srgb};

    let hue_deg = (hsl[0] as f32 / 255.0) * 360.0;
    let saturation = hsl[1] as f32 / 255.0;
    let lightness = hsl[2] as f32 / 255.0;

    let hsl_color = Hsl::new(hue_deg, saturation, lightness);
    let rgb_color: Srgb<f32> = Srgb::from_color(hsl_color);

    [
        (rgb_color.red * 255.0).round() as u8,
        (rgb_color.green * 255.0).round() as u8,
        (rgb_color.blue * 255.0).round() as u8,
        hsl[3],
    ]
}

pub(crate) fn render_colors_2d(
    image: &TaggedDistancePixelImage,
    index: usize,
    view: fidget::gui::View2,
    colors: &Color,
    out: &mut fidget::raster::Image<[u8; 4], fidget::render::ImageSize>,
) {
    let mat = view.world_to_model() * image.size().screen_to_world();
    let (colors, mode) = match colors {
        Color::Rgb(ts) => (ts.clone(), ColorMode::Rgb),
        Color::Hsl(ts) => (ts.clone(), ColorMode::Hsl),
    };

    let image_size = image.size();
    let mut ctx = fidget::Context::new();
    let colors = colors.map(|x| ctx.import(&x));

    let f = RenderFunction::new(&ctx, &colors).unwrap();
    let vars = f.vars();

    let mut tiles = vec![];
    const TILE_SIZE: u32 = 8;
    for y in 0..image_size.height().div_ceil(TILE_SIZE) {
        let y = y * TILE_SIZE;
        for x in 0..image_size.width().div_ceil(TILE_SIZE) {
            let x = x * TILE_SIZE;
            let mut any_inside = false;
            'outer: for dx in 0..TILE_SIZE {
                let x = x + dx;
                if x >= image_size.width() {
                    continue;
                }
                for dy in 0..TILE_SIZE {
                    let y = y + dy;
                    if y >= image_size.height() {
                        continue;
                    }
                    let p = image[(y as usize, x as usize)];
                    if p.1 == index && p.0.inside() {
                        any_inside = true;
                        break 'outer;
                    }
                }
            }
            if any_inside {
                tiles.push((x, y));
            }
        }
    }

    let tape = f.float_slice_tape(Default::default());

    let tiles = tiles
        .into_par_iter()
        .map_init(
            || {
                (
                    RenderFunction::new_float_slice_eval(),
                    vec![0f32; (TILE_SIZE * TILE_SIZE) as usize],
                    vec![0f32; (TILE_SIZE * TILE_SIZE) as usize],
                    vec![0f32; (TILE_SIZE * TILE_SIZE) as usize],
                )
            },
            |(eval, xs, ys, zs), (px, py)| {
                let mut i = 0;
                for dy in 0..TILE_SIZE {
                    for dx in 0..TILE_SIZE {
                        let pos = mat.transform_point(&nalgebra::Point2::new(
                            (px + dx) as f32,
                            (py + dy) as f32,
                        ));
                        xs[i] = pos.x;
                        ys[i] = pos.y;
                        i += 1;
                    }
                }
                // Dummy values, which we have to shuffle around
                let mut vs = [xs.as_slice(), ys.as_slice(), zs.as_slice()];
                if let Some(ix) = vars.get(&fidget::var::Var::X) {
                    vs[ix] = xs;
                }
                if let Some(iy) = vars.get(&fidget::var::Var::Y) {
                    vs[iy] = ys;
                }
                if let Some(iz) = vars.get(&fidget::var::Var::Z) {
                    vs[iz] = zs;
                }
                let out = eval.eval(&tape, &vs).unwrap();
                let r = &out[0];
                let g = &out[1];
                let b = &out[2];
                let image = (0..(TILE_SIZE as usize).pow(2))
                    .map(|i| [r[i], g[i], b[i], 1.0])
                    .collect::<Vec<_>>();
                (px, py, image)
            },
        )
        .collect::<Vec<_>>();

    for (x, y, data) in tiles {
        let mut iter = data.iter();
        for dy in 0..TILE_SIZE {
            for dx in 0..TILE_SIZE {
                let p = iter.next().unwrap();
                let x = x + dx;
                let y = y + dy;
                if x < image_size.width() && y < image_size.height() {
                    let d = image[(y as usize, x as usize)];
                    if d.0.inside() && d.1 == index {
                        let p = p.map(|p| (p.clamp(0.0, 1.0) * 255.0) as u8);
                        out[(y as usize, x as usize)] = match mode {
                            ColorMode::Rgb => p,
                            ColorMode::Hsl => hsl_to_rgb(p),
                        };
                    }
                }
            }
        }
    }
}

enum ColorMode {
    Rgb,
    Hsl,
}

/// Renders and accumulates a single index worth of colors
fn render_colors_3d(
    image: &TaggedGeometryPixelImage,
    index: usize,
    view: fidget::gui::View3,
    colors: &Color,
    out: &mut fidget::raster::Image<[u8; 4], fidget::render::VoxelSize>,
) {
    let mat = view.world_to_model() * image.size().screen_to_world();
    let (colors, mode) = match colors {
        Color::Rgb(ts) => (ts.clone(), ColorMode::Rgb),
        Color::Hsl(ts) => (ts.clone(), ColorMode::Hsl),
    };

    let image_size = image.size();
    let mut ctx = fidget::Context::new();
    let colors = colors.map(|x| ctx.import(&x));

    let f = RenderFunction::new(&ctx, &colors).unwrap();
    let vars = f.vars();

    let mut tiles = vec![];
    const TILE_SIZE: u32 = 8;
    for y in 0..image_size.height().div_ceil(TILE_SIZE) {
        let y = y * TILE_SIZE;
        for x in 0..image_size.width().div_ceil(TILE_SIZE) {
            let x = x * TILE_SIZE;
            let mut any_inside = false;
            'outer: for dx in 0..TILE_SIZE {
                let x = x + dx;
                if x >= image_size.width() {
                    continue;
                }
                for dy in 0..TILE_SIZE {
                    let y = y + dy;
                    if y >= image_size.height() {
                        continue;
                    }
                    let p = image[(y as usize, x as usize)];
                    if p.1 == index && p.0.depth != 0 {
                        any_inside = true;
                        break 'outer;
                    }
                }
            }
            if any_inside {
                tiles.push((x, y));
            }
        }
    }

    let tape = f.float_slice_tape(Default::default());

    let tiles = tiles
        .into_par_iter()
        .map_init(
            || {
                (
                    RenderFunction::new_float_slice_eval(),
                    vec![0f32; (TILE_SIZE * TILE_SIZE) as usize],
                    vec![0f32; (TILE_SIZE * TILE_SIZE) as usize],
                    vec![0f32; (TILE_SIZE * TILE_SIZE) as usize],
                )
            },
            |(eval, xs, ys, zs), (px, py)| {
                let mut i = 0;
                for dy in 0..TILE_SIZE {
                    for dx in 0..TILE_SIZE {
                        let px = (px + dx) as usize;
                        let py = (py + dy) as usize;
                        let pz = if py < image.height() && px < image.width() {
                            image[(py, px)].0.depth
                        } else {
                            0
                        };
                        let pos = mat.transform_point(&nalgebra::Point3::new(
                            px as f32, py as f32, pz as f32,
                        ));
                        xs[i] = pos.x;
                        ys[i] = pos.y;
                        zs[i] = pos.z;
                        i += 1;
                    }
                }
                // Dummy values, which we have to shuffle around
                let mut vs = [xs.as_slice(), ys.as_slice(), zs.as_slice()];
                if let Some(ix) = vars.get(&fidget::var::Var::X) {
                    vs[ix] = xs;
                }
                if let Some(iy) = vars.get(&fidget::var::Var::Y) {
                    vs[iy] = ys;
                }
                if let Some(iz) = vars.get(&fidget::var::Var::Z) {
                    vs[iz] = zs;
                }
                let out = eval.eval(&tape, &vs).unwrap();
                let r = &out[0];
                let g = &out[1];
                let b = &out[2];
                let image = (0..(TILE_SIZE as usize).pow(2))
                    .map(|i| [r[i], g[i], b[i], 1.0])
                    .collect::<Vec<_>>();
                (px, py, image)
            },
        )
        .collect::<Vec<_>>();

    for (x, y, data) in tiles {
        let mut iter = data.iter();
        for dy in 0..TILE_SIZE {
            for dx in 0..TILE_SIZE {
                let p = iter.next().unwrap();
                let x = x + dx;
                let y = y + dy;
                if x < image_size.width()
                    && y < image_size.height()
                    && image[(y as usize, x as usize)].1 == index
                {
                    let p = p.map(|p| (p.clamp(0.0, 1.0) * 255.0) as u8);
                    out[(y as usize, x as usize)] = match mode {
                        ColorMode::Rgb => p,
                        ColorMode::Hsl => hsl_to_rgb(p),
                    };
                }
            }
        }
    }
}

/// Convert a distance image into a bitfield image, with denoising
///
/// Filled pixels are normally converted to ±∞, but this can cause glitches
/// if they're on the edge of the model: linear interpolation in the texture
/// unit means that any pixel touching the infinite pixel will also be
/// infinite.
///
/// Denoising converts those infinite pixels into the average of their
/// neighbors, to reduce visual glitches when rendering lower-than-native
/// resolution images.
// TODO(fidget) Add this to effects?
fn denoise_2d(
    image: TaggedDistancePixelImage,
    threads: Option<&fidget::render::ThreadPool>,
) -> fidget::raster::Image<f32> {
    let mut out = fidget::raster::Image::new(image.size());
    out.apply_effect(
        |x: usize, y: usize| match image[(y, x)].0.unpack() {
            fidget::raster::pixel::DistancePixel::Value(v) => v,
            fidget::raster::pixel::DistancePixel::Fill { inside, .. } => {
                // Replace fill pixels with the average of their
                // actual-distance neighbors, falling back to infinity if
                // that fails.  This prevents glitchiness on the edges of
                // models.  If a fill pixel is exactly at the edge of a
                // model, linear interpolation in the texture means that
                // every pixel interpolated with the infinite pixel is also
                // infinite.
                let mut inside_count = 0;
                let mut inside_avg = 0.0;
                let mut outside_count = 0;
                let mut outside_avg = 0.0;
                for dx in [-1, 0, 1] {
                    let Some(x) = x.checked_add_signed(dx) else {
                        continue;
                    };
                    if x >= image.width() {
                        continue;
                    }
                    for dy in [-1, 0, 1] {
                        let Some(y) = y.checked_add_signed(dy) else {
                            continue;
                        };
                        if y >= image.height() {
                            continue;
                        }
                        if let Some(d) = image[(y, x)].0.distance() {
                            if d < 0.0 {
                                inside_avg += d;
                                inside_count += 1;
                            } else if d > 0.0 {
                                outside_avg += d;
                                outside_count += 1;
                            }
                        }
                    }
                }
                if inside && inside_count > 0 {
                    inside_avg / inside_count as f32
                } else if !inside && outside_count > 0 {
                    outside_avg / outside_count as f32
                } else if inside_count + outside_count > 0 {
                    (inside_avg + outside_avg)
                        / (inside_count + outside_count) as f32
                } else if inside {
                    -f32::INFINITY
                } else {
                    f32::INFINITY
                }
            }
        },
        threads,
    );
    out
}
