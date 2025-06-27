//! Image rendering
use crate::{
    view::{
        BitfieldViewImage, DebugViewImage, HeightmapViewImage, ImageData,
        SdfViewImage, ShadedViewImage, SsaoImageData, ViewCanvas, ViewImage,
        ViewMode2, ViewMode3,
    },
    world::Scene,
    BlockIndex, Message, MessageQueue,
};

use fidget::render::effects;

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
    pub fn spawn(
        block: BlockIndex,
        generation: u64,
        settings: RenderSettings,
        level: usize,
        tx: MessageQueue,
    ) -> Self {
        let cancel = fidget::render::CancelToken::new();
        let cancel_ = cancel.clone();
        let settings_ = settings.clone();
        let start_time = std::time::Instant::now();
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
                        Some((data, shape.color))
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
                                    let image = BitfieldViewImage::denoise(
                                        image, threads,
                                    );
                                    ImageData {
                                        data: image.take().0,
                                        color,
                                    }
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
                                .map(|(image, color)| {
                                    let image = effects::to_debug_bitmap(
                                        image, threads,
                                    );
                                    ImageData {
                                        data: image.take().0,
                                        color,
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
                        Some((data, shape.color))
                    })
                    .collect::<Option<_>>()?;
                match mode {
                    ViewMode3::Heightmap => {
                        let max = images
                            .iter()
                            .flat_map(|(image, _color)| image.iter())
                            .map(|v| v.depth)
                            .max()
                            .unwrap_or(0)
                            .max(1);
                        let image = HeightmapViewImage {
                            view: *view,
                            size: *size,
                            level,
                            data: images
                                .into_iter()
                                .map(|(image, color)| {
                                    let data = image
                                        .map(|v| ((v.depth * 255) / max) as u8)
                                        .take()
                                        .0;
                                    ImageData { data, color }
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
                                    // XXX this should all happen on the GPU,
                                    // probably!
                                    let image = effects::denoise_normals(
                                        &image, threads,
                                    );
                                    let ssao = effects::blur_ssao(
                                        &effects::compute_ssao(&image, threads),
                                        threads,
                                    );
                                    let (pixels, _size) = image.take();
                                    let (ssao, _size) = ssao.take();
                                    SsaoImageData {
                                        pixels,
                                        ssao,
                                        color,
                                    }
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
