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
        BitfieldImageData, BitfieldViewImage, HeightmapImageData,
        HeightmapViewImage, SdfImageData, SdfViewImage, ShadedImageData,
        ShadedViewImage, ViewCanvas, ViewImage, ViewMode2, ViewMode3,
    },
    world::{Color, Scene},
};

use fidget::{
    eval::{BulkEvaluator, Function, MathFunction},
    raster::{effects, pixel::DistancePixel, voxel::GeometryPixel},
};

use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
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
    Gpu { settings: VoxelRenderSettings },
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
            RenderTaskKind::Gpu { settings, .. } => match other {
                RenderSettings::Voxel(v) => v != settings,
                RenderSettings::Image(..) => true,
            },
        };
        settings_changed && self.level != max_level
    }
}

/// Dummy object representing a CPU render task
///
/// The actual task is spawned into the Rayon thread pool, so there's nothing to
/// be stored here.
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
                let image_size = fidget::render::ImageSize::new(
                    (size.width() / scale).max(1),
                    (size.height() / scale).max(1),
                );
                let cfg = fidget::raster::pixel::RenderConfig {
                    image_size,
                    world_to_model: view.world_to_model(),
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
                }
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
                let cfg = fidget::raster::voxel::RenderConfig {
                    image_size,
                    world_to_model,
                    cancel,
                    ..Default::default()
                };
                let images: Vec<_> = scene
                    .shapes
                    .par_iter()
                    .map(|shape| {
                        let rs = RenderShape::from(shape.tree.clone());
                        let data = cfg.run(rs)?;
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
                match mode {
                    ViewMode3::Heightmap => {
                        let image = HeightmapViewImage {
                            view: *view,
                            size: *size,
                            level,
                            data: images
                                .into_par_iter()
                                .map(|(image, color)| {
                                    image_to_heightmap(image, *view, color)
                                })
                                .collect(),
                        };
                        ViewImage::Heightmap(image)
                    }
                    ViewMode3::Shaded => {
                        let ssao = merged_ssao(&images);
                        let image = ShadedViewImage {
                            view: *view,
                            size: *size,
                            level,
                            ssao,
                            data: images
                                .into_par_iter()
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

/// Data required to render a scene on the GPU
///
/// This is constructed on the main thread then sent to the GPU worker pool to
/// be rendered off the main thread.
pub struct GpuRenderTask<N: Notify> {
    start_time: Instant,
    settings: VoxelRenderSettings,
    level: usize,
    block: BlockIndex,
    generation: u64,
    reply: MessageGenSender<N>,
}

/// Single shape rendering task, which renders a single task then sends it back
pub(crate) struct GpuRenderShapeTask {
    pub shape: fidget::context::Tree,
    pub config: fidget::wgpu::voxel::RenderConfig,
    pub image_size: fidget::render::VoxelSize,
    pub reply: flume::Sender<fidget::raster::voxel::Image>,
}

impl<N: Notify> GpuRenderTask<N> {
    /// Returns the render configuration and size
    pub fn cfg(
        &self,
    ) -> (fidget::wgpu::voxel::RenderConfig, fidget::render::VoxelSize) {
        let scale = 1 << self.level;

        // If this is our final rendering level, then do oversampling in
        // the Z direction for better rendering of edges.  XXX if you
        // change this, then you also need to edit `shaded.rs` to adjust
        // the `max_depth` passed into the shader.
        let bonus_z = if self.level == 0 { 2 } else { 1 };
        let image_size = fidget::render::VoxelSize::new(
            (self.settings.size.width() / scale).max(1),
            (self.settings.size.height() / scale).max(1),
            (self.settings.size.depth() / scale).max(1) * bonus_z,
        );
        let z_scale = 2.0 / bonus_z as f32;
        let scale = nalgebra::Scale3::new(1.0, 1.0, z_scale);
        let mut world_to_model =
            self.settings.view.world_to_model() * scale.to_homogeneous();
        if self.settings.perspective {
            *world_to_model.get_mut((3, 2)).unwrap() = 0.3 / bonus_z as f32;
        }
        (
            fidget::wgpu::voxel::RenderConfig { world_to_model },
            image_size,
        )
    }

    /// Post-processes images and sends them to the main thread
    pub fn finalize(
        self,
        mut images: Vec<(fidget::raster::voxel::Image, Option<Color>)>,
    ) {
        // Compensate for z-flattening
        let bonus_z = if self.level == 0 { 2 } else { 1 };
        let z_scale = 2.0 / bonus_z as f32;
        images.par_iter_mut().for_each(|(image, _)| {
            *image = image.map(|p| GeometryPixel {
                depth: p.depth,
                normal: [p.normal[0], p.normal[1], p.normal[2] / z_scale],
            });
        });

        log::info!(
            "rendered {:?} {} in {:?}",
            self.settings.size,
            self.level,
            self.start_time.elapsed()
        );
        let data = match self.settings.mode {
            ViewMode3::Heightmap => {
                let image = HeightmapViewImage {
                    view: self.settings.view,
                    size: self.settings.size,
                    level: self.level,
                    data: images
                        .into_par_iter()
                        .map(|(image, color)| {
                            image_to_heightmap(image, self.settings.view, color)
                        })
                        .collect(),
                };
                ViewImage::Heightmap(image)
            }
            ViewMode3::Shaded => {
                let ssao = merged_ssao(&images);
                let image = ShadedViewImage {
                    view: self.settings.view,
                    size: self.settings.size,
                    level: self.level,
                    ssao,
                    data: images
                        .into_par_iter()
                        .map(|(image, color)| {
                            image_to_shaded(image, self.settings.view, color)
                        })
                        .collect(),
                };
                ViewImage::Shaded(image)
            }
        };
        self.reply.send(Message::RenderView(RenderViewReply {
            block: self.block,
            generation: self.generation,
            start_time: self.start_time,
            settings: RenderSettings::Voxel(self.settings),
            data,
        }))
    }
}

#[derive(Default)]
pub(crate) struct GpuCache {
    shape_pool: Cache<
        *const fidget::context::TreeOp,
        fidget::wgpu::voxel::RenderShape,
        16,
    >,
}

/// Simple cache which contains `N` items
struct Cache<K, V, const N: usize> {
    /// Map from key to `(recency, value)` tuple
    ///
    /// `recency` is an upcounting `u64`, which will never roll over under all
    /// reasonable circumstances.
    data: HashMap<K, (u64, V)>,

    /// Map from recency to the relevant key
    recency: BTreeMap<u64, K>,
}

impl<K, V, const N: usize> Default for Cache<K, V, N> {
    fn default() -> Self {
        Self {
            data: HashMap::default(),
            recency: BTreeMap::default(),
        }
    }
}

impl<K, V, const N: usize> Cache<K, V, N>
where
    K: Copy + std::hash::Hash + Eq + std::fmt::Debug,
{
    /// Gets or inserts a value, keeping the cache to the target size
    fn get_or_insert_with<F: FnOnce() -> V>(&mut self, k: K, f: F) -> &V {
        // Raise a compile-time error if the size is invalid
        const { assert!(N > 0, "cache size N cannot be 0") };

        // Check invariants
        assert_eq!(self.data.len(), self.recency.len());

        // Find a larger recency value, which we'll use for this key
        let r = self
            .recency
            .last_key_value()
            .map(|(k, _v)| k)
            .cloned()
            .unwrap_or(0)
            + 1;

        match self.data.entry(k) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert((r, f()));
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let k_ = self.recency.remove(&o.get().0).expect("missing key");
                assert_eq!(k, k_);
                o.get_mut().0 = r; // update recency
            }
        }
        self.recency.insert(r, k);

        // We won't go below 0 items, to avoid removing the one we just added
        while self.data.len() > N {
            if let Some((r, k)) = self.recency.pop_first() {
                let (r_, _v) = self.data.remove(&k).expect("missing value");
                assert_eq!(r_, r, "incorrect recency");
            }
        }

        &self.data[&k].1
    }
}

impl GpuCache {
    /// Gets a shape and render buffers from the cache
    ///
    /// This is a single function because it must take `&mut self`
    pub(crate) fn get(
        &mut self,
        ctx: &mut fidget::wgpu::voxel::Context,
        d: &fidget::context::Tree,
    ) -> &fidget::wgpu::voxel::RenderShape {
        let key = d.as_ptr();
        self.shape_pool.get_or_insert_with(key, || {
            let rs = fidget::vm::VmShape::from(d.clone());

            // TODO check for bytecode feasibility earlier?
            // TODO fallback to CPU renderer
            log::info!("  building shape!");
            ctx.shape(&rs).unwrap()
        })
    }
}

/// The GPU worker pool is accessed with an MPMC channel
pub(crate) struct GpuWorkerPool {
    tx: flume::Sender<GpuRenderShapeTask>,
}

impl GpuWorkerPool {
    /// Builds a new worker pool which sends on the given channel
    pub fn new(tx: flume::Sender<GpuRenderShapeTask>) -> Self {
        Self { tx }
    }

    /// Begins a new image rendering task in the GPU global thread pool
    pub(crate) fn spawn<N: Notify>(
        &self,
        block: BlockIndex,
        generation: u64,
        settings: VoxelRenderSettings,
        level: usize,
        reply: MessageGenSender<N>,
    ) -> RenderTaskHandle {
        let settings_ = settings.clone();
        let start_time = Instant::now();
        let cancel = fidget::render::CancelToken::new();
        let task = GpuRenderTask {
            settings,
            level,
            block,
            generation,
            reply,
            start_time,
        };
        let (config, image_size) = task.cfg();
        let mut replies = vec![];
        for drawable in &task.settings.scene.shapes {
            let (tx, rx) = flume::bounded(0);
            self.tx
                .send(GpuRenderShapeTask {
                    shape: drawable.tree.clone(),
                    config,
                    image_size,
                    reply: tx,
                })
                .unwrap();
            replies.push((rx, drawable.color.clone()));
        }

        // Wait for the render to complete in the rayon pool
        rayon::spawn(move || {
            let images = replies
                .into_iter()
                .map(|(rx, color)| (rx.recv().unwrap(), color))
                .collect::<Vec<_>>();
            task.finalize(images);
        });

        RenderTaskHandle {
            kind: RenderTaskKind::Gpu {
                settings: settings_,
            },
            level,
            cancel,
        }
    }
}

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

fn image_to_sdf(
    image: fidget::raster::pixel::Image,
    view: fidget::gui::View2,
    color: Option<Color>,
) -> SdfImageData {
    let color = color.map(|c| {
        match c {
            Color::Rgb(rgb) => render_colors_2d(&image, view, rgb),
            Color::Hsl(hsl) => render_hsl_2d(&image, view, hsl),
        }
        .take()
        .0
        .into()
    });
    let distance = image
        .map(|d| match d.unpack() {
            DistancePixel::Value(d) => {
                if d.is_infinite() {
                    1e12f32.copysign(d)
                } else {
                    d
                }
            }
            DistancePixel::Fill { .. } => {
                panic!("expected all `Value` pixels")
            }
        })
        .take()
        .0
        .into();

    SdfImageData { distance, color }
}

pub(crate) fn image_to_bitfield(
    image: fidget::raster::pixel::Image,
    view: fidget::gui::View2,
    color: Option<Color>,
) -> BitfieldImageData {
    let threads = Some(&fidget::render::ThreadPool::Global);
    let color = color.map(|c| {
        match c {
            Color::Rgb(rgb) => render_colors_2d(&image, view, rgb),
            Color::Hsl(hsl) => render_hsl_2d(&image, view, hsl),
        }
        .take()
        .0
        .into()
    });
    let distance = BitfieldViewImage::denoise(image, threads).take().0.into();
    BitfieldImageData { distance, color }
}

fn image_to_heightmap(
    image: fidget::raster::Image<
        fidget::raster::voxel::GeometryPixel,
        fidget::render::VoxelSize,
    >,
    view: fidget::gui::View3,
    color: Option<Color>,
) -> HeightmapImageData {
    let color = color.map(|c| {
        match c {
            Color::Rgb(rgb) => render_colors_3d(&image, view, rgb),
            Color::Hsl(hsl) => render_hsl_3d(&image, view, hsl),
        }
        .take()
        .0
        .into()
    });
    let depth = image.map(|v| v.depth).take().0.into();
    HeightmapImageData { depth, color }
}

fn merged_ssao(
    images: &[(fidget::raster::voxel::Image, Option<Color>)],
) -> std::sync::Arc<[f32]> {
    let mut out = fidget::raster::voxel::Image::new(images[0].0.size());
    let threads = Some(&fidget::render::ThreadPool::Global);
    out.apply_effect(
        |x, y| {
            images
                .iter()
                .map(|(i, _c)| i[(y, x)])
                .max_by_key(|p| ordered_float::OrderedFloat(p.depth))
                .unwrap_or(GeometryPixel {
                    depth: 0.0,
                    normal: [0.0; 3],
                })
        },
        threads,
    );
    let ssao =
        effects::blur_ssao(&effects::compute_ssao(&out, threads), threads);
    ssao.take().0.into()
}

fn image_to_shaded(
    image: fidget::raster::voxel::Image,
    view: fidget::gui::View3,
    color: Option<Color>,
) -> ShadedImageData {
    let threads = Some(&fidget::render::ThreadPool::Global);

    let color = color
        .map(|c| {
            match c {
                Color::Rgb(rgb) => render_colors_3d(&image, view, rgb),
                Color::Hsl(hsl) => render_hsl_3d(&image, view, hsl),
            }
            .take()
            .0
            .into()
        })
        .unwrap_or_else(|| {
            let pixel_count =
                image.size().width() as usize * image.size().height() as usize;
            vec![[u8::MAX; 4]; pixel_count].into()
        });

    // XXX this should all happen on the GPU, probably!
    let image = effects::denoise_normals(&image, threads);
    let pixels = image.take().0.into();
    ShadedImageData { pixels, color }
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

fn render_hsl_2d(
    image: &fidget::raster::pixel::Image,
    view: fidget::gui::View2,
    hsl: [fidget::context::Tree; 3],
) -> fidget::raster::Image<[u8; 4]> {
    let image = render_colors_2d(image, view, hsl);
    let mut out = fidget::raster::Image::new(image.size());
    out.apply_effect(
        |x, y| {
            let hsl = image[(y, x)];
            hsl_to_rgb(hsl)
        },
        Some(&fidget::render::ThreadPool::Global),
    );
    out
}

pub(crate) fn render_colors_2d(
    image: &fidget::raster::pixel::Image,
    view: fidget::gui::View2,
    colors: [fidget::context::Tree; 3],
) -> fidget::raster::Image<[u8; 4]> {
    let mat = view.world_to_model() * image.size().screen_to_world();

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

    let mut out = fidget::raster::Image::new(image_size);
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
    image: &fidget::raster::voxel::Image,
    view: fidget::gui::View3,
    hsl: [fidget::context::Tree; 3],
) -> fidget::raster::Image<[u8; 4], fidget::render::VoxelSize> {
    let image = render_colors_3d(image, view, hsl);
    let mut out = fidget::raster::Image::new(image.size());
    out.apply_effect(
        |x, y| {
            let hsl = image[(y, x)];
            hsl_to_rgb(hsl)
        },
        Some(&fidget::render::ThreadPool::Global),
    );
    out
}

fn render_colors_3d(
    image: &fidget::raster::voxel::Image,
    view: fidget::gui::View3,
    colors: [fidget::context::Tree; 3],
) -> fidget::raster::Image<[u8; 4], fidget::render::VoxelSize> {
    let mat = view.world_to_model() * image.size().screen_to_world();

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
                    if image[(y as usize, x as usize)].depth != 0.0 {
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
                            0.0
                        };
                        let pos = mat.transform_point(&nalgebra::Point3::new(
                            px as f32, py as f32, pz,
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

    let mut out = fidget::raster::Image::new(image_size);
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
