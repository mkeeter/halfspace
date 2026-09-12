//! Painter drawing SDFs and bitfields in a 2D view
use super::WgpuResources;

use crate::{
    painters::cache::{CacheHit, WgpuTextureCache},
    view::{PixelImage, ViewMode2},
    world::BlockIndex,
};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use fidget::raster::pixel::RawDistancePixel;
use std::collections::HashMap;
use zerocopy::IntoBytes;

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
struct Uniforms {
    transform: [[f32; 4]; 4],
    has_color: u32,
    _pad: [u32; 3],
}

/// GPU callback
pub struct WgpuSdfPainter {
    /// Current view, which may differ from the image's view
    view: fidget::gui::View2,
    size: fidget::render::ImageSize,

    /// Index of the block being rendered
    index: BlockIndex,

    /// Image to render
    image: PixelImage,
}

impl WgpuSdfPainter {
    /// Builds a new heightmap painter
    ///
    /// Note that `size` and `view` are associated with the current rendering
    /// quad; the `image` contains its own size and view transforms.
    pub fn new(
        index: BlockIndex,
        image: PixelImage,
        size: fidget::render::ImageSize,
        view: fidget::gui::View2,
    ) -> Self {
        Self {
            index,
            size,
            view,
            image,
        }
    }
}

/// Resources for drawing SDF (2D) images
///
/// There is a single copy of this resources object, and it's available during
/// both preparation and painting passes.
pub(crate) struct SdfResources {
    bitfield_pipeline: wgpu::RenderPipeline,
    sdf_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bound_data: HashMap<BlockIndex, SdfData>,

    color_cache: WgpuTextureCache<[[u8; 4]]>,
    distance_cache: WgpuTextureCache<[RawDistancePixel]>,

    /// Empty texture used when we don't have a color channel
    dummy_color_texture: wgpu::Texture,

    /// Pool of buffers which are sized to fit a [`Uniforms`] object
    config_buf_pool: Vec<wgpu::Buffer>,
}

impl SdfResources {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        // Create bind group layout (same for bitfield and SDF rendering)
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sdf bind group layout"),
                entries: &[
                    // Distance texture and sampler
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float {
                                filterable: true,
                            },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                    // Color texture and sampler
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float {
                                filterable: true,
                            },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                    // Uniforms
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        // Create render pipeline layouts
        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("sdf render pipeline layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0u32,
            });

        // Create the SDF render pipeline
        let sdf_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("sdf shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/shaders/sdf.wgsl"
                    ))
                    .into(),
                ),
            });
        let sdf_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("sdf render pipeline"),
                layout: Some(&pipeline_layout),
                cache: None,
                vertex: wgpu::VertexState {
                    module: &sdf_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &sdf_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent::OVER,
                            alpha: wgpu::BlendComponent::OVER,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
            });

        // Create the bitfield render pipeline
        let bitfield_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("bitfield shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/shaders/bitfield.wgsl"
                    ))
                    .into(),
                ),
            });
        let bitfield_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("bitfield render pipeline"),
                layout: Some(&pipeline_layout),
                cache: None,
                vertex: wgpu::VertexState {
                    module: &bitfield_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &bitfield_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent::OVER,
                            alpha: wgpu::BlendComponent::OVER,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
            });

        let dummy_color_texture =
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dummy color texture"),
                size: wgpu::Extent3d {
                    width: 32,
                    height: 32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });

        Self {
            sdf_pipeline,
            bitfield_pipeline,
            bind_group_layout,
            bound_data: HashMap::new(),
            distance_cache: WgpuTextureCache::new(),
            color_cache: WgpuTextureCache::new(),
            dummy_color_texture,
            config_buf_pool: vec![],
        }
    }

    pub fn reset(&mut self) {
        // Empty out the texture / buffer cache that weren't used last frame
        // (anything used last frame is in bound_data instead)
        self.color_cache.clear();
        self.distance_cache.clear();
        self.config_buf_pool.clear();

        // Move bound data into the caches, for possible reuse.  If it's not
        // used in the upcoming frame, then it's cleared next frame (above).
        for (_index, data) in self.bound_data.drain() {
            if let Some(c) = data.image.color {
                self.color_cache.insert(c, data.color_texture);
            }
            self.distance_cache
                .insert(data.image.distance, data.distance_texture);
            self.config_buf_pool.push(data.uniform_buffer);
        }
    }

    fn get_data(
        &mut self,
        image: &PixelImage,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: wgpu::Extent3d,
    ) -> SdfData {
        let (distance_texture, needs_write) =
            match self.distance_cache.get(&image.distance, size) {
                Some(CacheHit::DataMatch(tex)) => (tex, false),
                Some(CacheHit::SizeMatch(tex)) => (tex, true),
                None => (
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("sdf distance texture"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::R32Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    }),
                    true,
                ),
            };
        if needs_write {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &distance_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                image.distance.as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * size.width),
                    rows_per_image: Some(size.height),
                },
                size,
            );
        }
        let distance_texture_view =
            distance_texture.create_view(&Default::default());
        let distance_sampler =
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("sdf distance sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            });

        // If the image has a color channel, then get a color texture;
        // otherwise, just return the dummy texture (which will be unused in the
        // shader itself).
        let color_texture = if let Some(color) = &image.color {
            let (color_texture, needs_write) =
                match self.color_cache.get(color, size) {
                    Some(CacheHit::DataMatch(tex)) => (tex, false),
                    Some(CacheHit::SizeMatch(tex)) => (tex, true),
                    None => (
                        device.create_texture(&wgpu::TextureDescriptor {
                            label: Some("sdf color texture"),
                            size,
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING
                                | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        }),
                        true,
                    ),
                };
            if needs_write {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &color_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    color.as_bytes(),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * size.width),
                        rows_per_image: Some(size.height),
                    },
                    size,
                );
            }
            color_texture
        } else {
            self.dummy_color_texture.clone()
        };

        let color_texture_view = color_texture.create_view(&Default::default());
        let color_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sdf color sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = self.config_buf_pool.pop().unwrap_or_else(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("uniform buffer"),
                size: std::mem::size_of::<Uniforms>() as u64,
                mapped_at_creation: false,
                usage: wgpu::BufferUsages::UNIFORM
                    | wgpu::BufferUsages::COPY_DST,
            })
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sdf bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &distance_texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&distance_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(
                        &color_texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&color_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });

        SdfData {
            image: image.clone(),
            distance_texture,
            color_texture,
            bind_group,
            uniform_buffer,
        }
    }

    fn paint_sdf(&self, render_pass: &mut wgpu::RenderPass, sdf: &SdfData) {
        render_pass.set_pipeline(&self.sdf_pipeline);
        render_pass.set_bind_group(0, &sdf.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }

    fn paint_bitfield(
        &self,
        render_pass: &mut wgpu::RenderPass,
        sdf: &SdfData,
    ) {
        render_pass.set_pipeline(&self.bitfield_pipeline);
        render_pass.set_bind_group(0, &sdf.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}

/// Resources used to render a SDF
struct SdfData {
    /// Source image (used for pointer-based texture reuse)
    image: PixelImage,

    /// Distance texture (`f32`)
    distance_texture: wgpu::Texture,

    /// Color texture (`Rgba8Unorm`)
    color_texture: wgpu::Texture,

    /// Uniform buffer
    uniform_buffer: wgpu::Buffer,

    /// Bind group for SDF rendering
    bind_group: wgpu::BindGroup,
}

impl egui_wgpu::CallbackTrait for WgpuSdfPainter {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let gr: &mut WgpuResources = resources.get_mut().unwrap();

        let image_size = self.image.size;
        let width = (image_size.width() / (1 << self.image.level)).max(1);
        let height = (image_size.height() / (1 << self.image.level)).max(1);
        let texture_size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let transform = super::transform2(
            self.image.view,
            self.image.size,
            self.view,
            self.size,
        );

        let data = gr.sdf.get_data(&self.image, device, queue, texture_size);

        let uniforms = Uniforms {
            transform: transform.into(),
            has_color: u32::from(self.image.color.is_some()),
            _pad: [0; _],
        };
        {
            let mut writer = queue
                .write_buffer_with(
                    &data.uniform_buffer,
                    0,
                    (std::mem::size_of_val(&uniforms) as u64)
                        .try_into()
                        .unwrap(),
                )
                .unwrap();
            writer.copy_from_slice(uniforms.as_bytes());
        }

        let prev = gr.sdf.bound_data.insert(self.index, data);
        assert!(prev.is_none());

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let rs: &WgpuResources = resources.get().unwrap();
        let data = &rs.sdf.bound_data[&self.index];

        rs.clear.paint(render_pass);
        match data.image.mode {
            ViewMode2::Sdf => rs.sdf.paint_sdf(render_pass, data),
            ViewMode2::Bitfield => rs.sdf.paint_bitfield(render_pass, data),
        }
    }
}
