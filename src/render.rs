//! Image rendering
use crate::{
    view::{
        ImageData, ViewCanvas, ViewData2, ViewData3, ViewImage, ViewMode2,
        ViewMode3,
    },
    world::Scene,
    BlockIndex, Message,
};

#[cfg(feature = "jit")]
type RenderShape = fidget::jit::JitShape;

#[cfg(not(feature = "jit"))]
type RenderShape = fidget::vm::VmShape;

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
    pub fn spawn<F: FnOnce() + Send + Sync + 'static>(
        block: BlockIndex,
        generation: u64,
        settings: RenderSettings,
        level: usize,
        tx: std::sync::mpsc::Sender<Message>,
        notify: F,
    ) -> Self {
        let cancel = fidget::render::CancelToken::new();
        let cancel_ = cancel.clone();
        let settings_ = settings.clone();
        let start_time = std::time::Instant::now();
        rayon::spawn(move || {
            if let Some(data) = Self::run(&settings_, level, cancel_) {
                if tx
                    .send(Message::RenderView {
                        block,
                        generation,
                        start_time,
                        data,
                    })
                    .is_ok()
                {
                    notify();
                }
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
                        Some((data, shape.color))
                    })
                    .collect::<Option<_>>()?;

                let data = match mode {
                    ViewMode2::Bitfield => ViewData2::Bitfield(
                        images
                            .into_iter()
                            .map(|(image, color)| {
                                let image = image.map(|d| match d.distance() {
                                    Ok(d) => d,
                                    Err(e) => {
                                        if e.inside {
                                            -f32::INFINITY
                                        } else {
                                            f32::INFINITY
                                        }
                                    }
                                });
                                ImageData {
                                    data: image.take().0,
                                    color,
                                }
                            })
                            .collect(),
                    ),
                    ViewMode2::Sdf => ViewData2::Sdf(
                        images
                            .into_iter()
                            .map(|(image, color)| {
                                let image = image.map(|d| {
                                    let d = d.distance().unwrap();
                                    if d.is_infinite() {
                                        1e12f32.copysign(d)
                                    } else {
                                        d
                                    }
                                });
                                ImageData {
                                    data: image.take().0,
                                    color,
                                }
                            })
                            .collect(),
                    ),
                };
                ViewImage::View2 {
                    data,
                    view: *view,
                    size: *size,
                    level,
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
                        Some((data, shape.color))
                    })
                    .collect::<Option<_>>()?;
                let data = match mode {
                    ViewMode3::Heightmap => {
                        let max = images
                            .iter()
                            .flat_map(|(image, _color)| image.iter())
                            .map(|v| v.depth)
                            .max()
                            .unwrap_or(0)
                            .max(1);
                        ViewData3::Heightmap(
                            images
                                .into_iter()
                                .map(|(image, color)| {
                                    let data = image
                                        .map(|v| ((v.depth * 255) / max) as u8)
                                        .take()
                                        .0;
                                    ImageData { data, color }
                                })
                                .collect(),
                        )
                    }
                    ViewMode3::Shaded => ViewData3::Shaded(
                        // XXX this should all happen on the GPU, probably
                        images
                            .into_iter()
                            .map(|(image, color)| {
                                let threads =
                                    Some(&fidget::render::ThreadPool::Global);
                                let image =
                                    fidget::render::effects::denoise_normals(
                                        &image, threads,
                                    );
                                let shaded =
                                    fidget::render::effects::apply_shading(
                                        &image, true, threads,
                                    );
                                let mut out: fidget::render::Image<[u8; 4], _> =
                                    fidget::render::Image::new(image_size);
                                out.apply_effect(
                                    |x, y| {
                                        let p = image[(y, x)];
                                        if p.depth > 0 {
                                            let c = shaded[(y, x)];
                                            [c[0], c[1], c[2], 255]
                                        } else {
                                            [0, 0, 0, 0]
                                        }
                                    },
                                    threads,
                                );
                                let (data, _size) = out.take();
                                ImageData { data, color }
                            })
                            .collect(),
                    ),
                };
                ViewImage::View3 {
                    data,
                    view: *view,
                    size: *size,
                    level,
                }
            }
        };
        Some(data)
    }
}

/// Settings for rendering an image
#[derive(Clone)]
pub enum RenderSettings {
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
        // XXX this does expensive tree deduplication!
        let mut ctx = fidget::Context::new();
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
                    && scene_a.shapes.iter().zip(&scene_b.shapes).all(
                        |(a, b)| {
                            a.color == b.color
                                && ctx.import(&a.tree) == ctx.import(&b.tree)
                        },
                    )
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
                    && scene_a.shapes.iter().zip(&scene_b.shapes).all(
                        |(a, b)| {
                            a.color == b.color
                                && ctx.import(&a.tree) == ctx.import(&b.tree)
                        },
                    )
            }
            _ => false,
        }
    }
}
