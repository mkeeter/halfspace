//! Image rendering
use crate::{
    BlockIndex, Message, MessageSender,
    view::{
        BitfieldImageData, BitfieldViewImage, DebugImageData, DebugViewImage,
        HeightmapImageData, HeightmapViewImage, SdfImageData, SdfViewImage,
        ShadedImageData, ShadedViewImage, ViewCanvas, ViewImage, ViewMode2,
        ViewMode3,
    },
    world::{Color, Scene},
};

use fidget::{
    eval::{BulkEvaluator, Function, MathFunction},
    render::effects,
};

use rayon::prelude::*;
use web_time::Instant;

#[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
type RenderFunction = fidget::jit::JitFunction;

#[cfg(any(target_arch = "wasm32", not(feature = "jit")))]
type RenderFunction = fidget::vm::VmFunction;

type RenderShape = fidget::shape::Shape<RenderFunction>;

/// State representing an in-progress render
pub struct RenderTask {
    settings: RenderSettings,
    level: usize,
    done: bool,
    cancel: fidget::render::CancelToken,
}

impl Drop for RenderTask {
    fn drop(&mut self) {
        self.cancel.cancel()
    }
}

impl RenderTask {
    /// Checks whether the `done` flag is set
    pub fn done(&self) -> bool {
        self.done
    }

    /// Sets the `done` flag
    pub fn set_done(&mut self) {
        self.done = true
    }

    /// Checks whether the new settings are different from our settings
    ///
    /// This only returns `true` if `self.level != max_level`; we want to avoid
    /// interrupting max-level renders to preserve responsiveness.
    pub fn should_cancel(
        &self,
        other: &RenderSettings,
        max_level: usize,
    ) -> bool {
        &self.settings != other && (self.done || self.level != max_level)
    }

    /// Begins a new image rendering task in the global thread pool
    pub(crate) fn spawn(
        block: BlockIndex,
        generation: u64,
        settings: RenderSettings,
        level: usize,
        tx: MessageSender,
    ) -> Self {
        let cancel = fidget::render::CancelToken::new();
        let cancel_ = cancel.clone();
        let settings_ = settings.clone();
        let start_time = Instant::now();
        rayon::spawn(move || {
            if let Some(data) = Self::run(&settings_, level, cancel_) {
                tx.send(Message::RenderView {
                    block,
                    generation,
                    start_time,
                    data,
                })
            }
        });
        Self {
            settings,
            cancel,
            level,
            done: false,
        }
    }

    /// Function which actually renders images (off-thread)
    pub fn run(
        settings: &RenderSettings,
        level: usize,
        cancel: fidget::render::CancelToken,
    ) -> Option<ViewImage> {
        let scale = 1 << level;
        let threads = Some(&fidget::render::ThreadPool::Global);
        let data = match settings {
            RenderSettings::Render2 {
                scene,
                mode,
                view,
                size,
            } => {
                let image_size = fidget::render::ImageSize::new(
                    (size.width() / scale).max(1),
                    (size.height() / scale).max(1),
                );
                let cfg = fidget::render::ImageRenderConfig {
                    image_size,
                    view: *view,
                    cancel,
                    pixel_perfect: matches!(mode, ViewMode2::Sdf),
                    ..Default::default()
                };
                let images: Vec<_> = scene
                    .shapes
                    .iter()
                    .map(|shape| {
                        let rs = RenderShape::from(shape.tree.clone());
                        let data = cfg.run(rs)?;
                        Some((data, shape.color.clone()))
                    })
                    .collect::<Option<_>>()?;

                match mode {
                    ViewMode2::Bitfield => {
                        let image = BitfieldViewImage {
                            view: *view,
                            size: *size,
                            level,
                            data: images
                                .into_iter()
                                .map(|(image, color)| {
                                    image_to_bitfield(image, *view, color)
                                })
                                .collect(),
                        };
                        ViewImage::Bitfield(image)
                    }

                    ViewMode2::Sdf => {
                        let image = SdfViewImage {
                            view: *view,
                            size: *size,
                            level,
                            data: images
                                .into_iter()
                                .map(|(image, color)| {
                                    image_to_sdf(image, *view, color)
                                })
                                .collect(),
                        };
                        ViewImage::Sdf(image)
                    }
                    ViewMode2::Debug => {
                        let image = DebugViewImage {
                            view: *view,
                            size: *size,
                            level,
                            data: images
                                .into_iter()
                                .map(|(image, _color)| {
                                    let image = effects::to_debug_bitmap(
                                        image, threads,
                                    );
                                    DebugImageData {
                                        pixels: image.take().0,
                                        // No color for debug images
                                    }
                                })
                                .collect(),
                        };
                        ViewImage::Debug(image)
                    }
                }
            }
            RenderSettings::Render3 {
                scene,
                mode,
                view,
                size,
            } => {
                let image_size = fidget::render::VoxelSize::new(
                    (size.width() / scale).max(1),
                    (size.height() / scale).max(1),
                    (size.depth() / scale).max(1),
                );
                let cfg = fidget::render::VoxelRenderConfig {
                    image_size,
                    view: *view,
                    cancel,
                    ..Default::default()
                };
                let images: Vec<_> = scene
                    .shapes
                    .iter()
                    .map(|shape| {
                        let rs = RenderShape::from(shape.tree.clone());
                        let data = cfg.run(rs)?;
                        Some((data, shape.color.clone()))
                    })
                    .collect::<Option<_>>()?;
                match mode {
                    ViewMode3::Heightmap => {
                        let image = HeightmapViewImage {
                            view: *view,
                            size: *size,
                            level,
                            data: images
                                .into_iter()
                                .map(|(image, color)| {
                                    image_to_heightmap(image, *view, color)
                                })
                                .collect(),
                        };
                        ViewImage::Heightmap(image)
                    }
                    ViewMode3::Shaded => {
                        let image = ShadedViewImage {
                            view: *view,
                            size: *size,
                            level,
                            data: images
                                .into_iter()
                                .map(|(image, color)| {
                                    image_to_shaded(image, *view, color)
                                })
                                .collect(),
                        };
                        ViewImage::Shaded(image)
                    }
                }
            }
        };
        Some(data)
    }
}

/// Settings for rendering an image
#[derive(Clone)]
pub enum RenderSettings {
    // TODO flatten this?
    Render2 {
        scene: Scene,
        mode: ViewMode2,
        view: fidget::render::View2,
        size: fidget::render::ImageSize,
    },
    Render3 {
        scene: Scene,
        mode: ViewMode3,
        view: fidget::render::View3,
        size: fidget::render::VoxelSize,
    },
}

impl RenderSettings {
    pub fn from_canvas(canvas: &ViewCanvas, scene: Scene) -> Self {
        match canvas {
            ViewCanvas::Canvas2 { canvas, mode } => RenderSettings::Render2 {
                scene,
                view: canvas.view(),
                size: canvas.image_size(),
                mode: *mode,
            },
            ViewCanvas::Canvas3 { canvas, mode } => {
                let size = canvas.image_size();
                RenderSettings::Render3 {
                    scene,
                    view: canvas.view(),
                    size: fidget::render::VoxelSize::new(
                        size.width(),
                        size.height(),
                        // XXX select depth?
                        size.width().max(size.height()),
                    ),
                    mode: *mode,
                }
            }
        }
    }
}

impl std::cmp::PartialEq for RenderSettings {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Render2 {
                    scene: scene_a,
                    mode: mode_a,
                    view: view_a,
                    size: size_a,
                },
                Self::Render2 {
                    scene: scene_b,
                    mode: mode_b,
                    view: view_b,
                    size: size_b,
                },
            ) => {
                mode_a == mode_b
                    && view_a == view_b
                    && size_a == size_b
                    && scene_a.shapes.len() == scene_b.shapes.len()
                    && scene_a
                        .shapes
                        .iter()
                        .zip(&scene_b.shapes)
                        .all(|(a, b)| a.color == b.color && a.tree == b.tree)
            }
            (
                Self::Render3 {
                    scene: scene_a,
                    mode: mode_a,
                    view: view_a,
                    size: size_a,
                },
                Self::Render3 {
                    scene: scene_b,
                    mode: mode_b,
                    view: view_b,
                    size: size_b,
                },
            ) => {
                mode_a == mode_b
                    && view_a == view_b
                    && size_a == size_b
                    && scene_a.shapes.len() == scene_b.shapes.len()
                    && scene_a
                        .shapes
                        .iter()
                        .zip(&scene_b.shapes)
                        .all(|(a, b)| a.color == b.color && a.tree == b.tree)
            }
            _ => false,
        }
    }
}

fn image_to_sdf(
    image: fidget::render::Image<fidget::render::DistancePixel>,
    view: fidget::render::View2,
    color: Option<Color>,
) -> SdfImageData {
    let color = color.map(|c| {
        match c {
            Color::Rgb(rgb) => render_rgb_2d(&image, view, rgb),
            Color::Hsl(hsl) => render_hsl_2d(&image, view, hsl),
        }
        .take()
        .0
    });
    let distance = image
        .map(|d| {
            let d = d.distance().unwrap();
            if d.is_infinite() {
                1e12f32.copysign(d)
            } else {
                d
            }
        })
        .take()
        .0;

    SdfImageData { distance, color }
}

fn image_to_bitfield(
    image: fidget::render::Image<fidget::render::DistancePixel>,
    view: fidget::render::View2,
    color: Option<Color>,
) -> BitfieldImageData {
    let threads = Some(&fidget::render::ThreadPool::Global);
    let color = color.map(|c| {
        match c {
            Color::Rgb(rgb) => render_rgb_2d(&image, view, rgb),
            Color::Hsl(hsl) => render_hsl_2d(&image, view, hsl),
        }
        .take()
        .0
    });
    let distance = BitfieldViewImage::denoise(image, threads).take().0;
    BitfieldImageData { distance, color }
}

fn image_to_heightmap(
    image: fidget::render::Image<
        fidget::render::GeometryPixel,
        fidget::render::VoxelSize,
    >,
    view: fidget::render::View3,
    color: Option<Color>,
) -> HeightmapImageData {
    let color = color.map(|c| {
        match c {
            Color::Rgb(rgb) => render_rgb_3d(&image, view, rgb),
            Color::Hsl(hsl) => render_hsl_3d(&image, view, hsl),
        }
        .take()
        .0
    });
    let depth = image.map(|v| v.depth as f32).take().0;
    HeightmapImageData { depth, color }
}

fn image_to_shaded(
    image: fidget::render::Image<
        fidget::render::GeometryPixel,
        fidget::render::VoxelSize,
    >,
    view: fidget::render::View3,
    color: Option<Color>,
) -> ShadedImageData {
    let threads = Some(&fidget::render::ThreadPool::Global);

    let color = color.map(|c| {
        match c {
            Color::Rgb(rgb) => render_rgb_3d(&image, view, rgb),
            Color::Hsl(hsl) => render_hsl_3d(&image, view, hsl),
        }
        .take()
        .0
    });

    // XXX this should all happen on the GPU, probably!
    let image = effects::denoise_normals(&image, threads);
    let ssao =
        effects::blur_ssao(&effects::compute_ssao(&image, threads), threads);
    let (pixels, _size) = image.take();
    let (ssao, _size) = ssao.take();
    ShadedImageData {
        pixels,
        ssao,
        color,
    }
}

fn hsl_to_rgb(hsl: [u8; 4]) -> [u8; 4] {
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

fn render_hsl_2d(
    image: &fidget::render::Image<fidget::render::DistancePixel>,
    view: fidget::render::View2,
    hsl: [fidget::context::Tree; 3],
) -> fidget::render::Image<[u8; 4]> {
    let image = render_rgb_2d(image, view, hsl);
    let mut out = fidget::render::Image::new(image.size());
    out.apply_effect(
        |x, y| {
            let hsl = image[(y, x)];
            hsl_to_rgb(hsl)
        },
        Some(&fidget::render::ThreadPool::Global),
    );
    out
}

fn render_rgb_2d(
    image: &fidget::render::Image<fidget::render::DistancePixel>,
    view: fidget::render::View2,
    rgb: [fidget::context::Tree; 3],
) -> fidget::render::Image<[u8; 4]> {
    let mat = view.world_to_model() * image.size().screen_to_world();

    let image_size = image.size();
    let mut ctx = fidget::Context::new();
    let rgb = rgb.map(|x| ctx.import(&x));

    let f = RenderFunction::new(&ctx, &rgb).unwrap();
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
                    if image[(y as usize, x as usize)].inside() {
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

    let mut out = fidget::render::Image::new(image_size);
    for (x, y, data) in tiles {
        let mut iter = data.iter();
        for dy in 0..TILE_SIZE {
            for dx in 0..TILE_SIZE {
                let p = iter.next().unwrap();
                let x = x + dx;
                let y = y + dy;
                if x < image_size.width() && y < image_size.height() {
                    out[(y as usize, x as usize)] =
                        p.map(|p| (p.clamp(0.0, 1.0) * 255.0) as u8);
                }
            }
        }
    }
    out
}

fn render_hsl_3d(
    image: &fidget::render::Image<
        fidget::render::GeometryPixel,
        fidget::render::VoxelSize,
    >,
    view: fidget::render::View3,
    hsl: [fidget::context::Tree; 3],
) -> fidget::render::Image<[u8; 4], fidget::render::VoxelSize> {
    let image = render_rgb_3d(image, view, hsl);
    let mut out = fidget::render::Image::new(image.size());
    out.apply_effect(
        |x, y| {
            let hsl = image[(y, x)];
            hsl_to_rgb(hsl)
        },
        Some(&fidget::render::ThreadPool::Global),
    );
    out
}

fn render_rgb_3d(
    image: &fidget::render::Image<
        fidget::render::GeometryPixel,
        fidget::render::VoxelSize,
    >,
    view: fidget::render::View3,
    rgb: [fidget::context::Tree; 3],
) -> fidget::render::Image<[u8; 4], fidget::render::VoxelSize> {
    let mat = view.world_to_model() * image.size().screen_to_world();

    let image_size = image.size();
    let mut ctx = fidget::Context::new();
    let rgb = rgb.map(|x| ctx.import(&x));

    let f = RenderFunction::new(&ctx, &rgb).unwrap();
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
                    if image[(y as usize, x as usize)].depth != 0 {
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
                            image[(py, px)].depth
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

    let mut out = fidget::render::Image::new(image_size);
    for (x, y, data) in tiles {
        let mut iter = data.iter();
        for dy in 0..TILE_SIZE {
            for dx in 0..TILE_SIZE {
                let p = iter.next().unwrap();
                let x = x + dx;
                let y = y + dy;
                if x < image_size.width() && y < image_size.height() {
                    out[(y as usize, x as usize)] =
                        p.map(|p| (p.clamp(0.0, 1.0) * 255.0) as u8);
                }
            }
        }
    }
    out
}
