use super::{blit::BlitData, WgpuResources};
use crate::{view::ShadedViewImage, world::BlockIndex};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use std::collections::HashMap;
use zerocopy::IntoBytes;

/// GPU callback for painting shaded objects
pub struct WgpuShadedPainter {
    /// Current view, which may differ from the image's view
    view: fidget::render::View3,
    size: fidget::render::ImageSize,

    /// Index of the block being rendered
    index: BlockIndex,

    /// Image(s) to draw to the screen
    image: ShadedViewImage,
}

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
struct Uniforms {
    transform: [[f32; 4]; 4],
    max_depth: u32,
    has_color: u32,
    _padding: [u8; 8],
}

impl WgpuShadedPainter {
    /// Builds a new shaded painter
    ///
    /// Note that `size` and `view` are associated with the current rendering
    /// quad; the `image` contains its own size and view transforms.
    pub fn new(
        index: BlockIndex,
        image: ShadedViewImage,
        size: fidget::render::ImageSize,
        view: fidget::render::View3,
    ) -> Self {
        Self {
            index,
            image,
            size,
            view,
        }
    }
}

impl egui_wgpu::CallbackTrait for WgpuShadedPainter {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        egui_encoder: &mut wgpu::CommandEncoder,
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

        let transform = super::transform3(
            self.image.view,
            self.image.size,
            self.view,
            self.size,
        );

        let blit =
            BlitData::new(device, &gr.blit.bind_group_layout, texture_size);
        gr.shaded.bound_data.insert(
            self.index,
            ShadedBundleData {
                blit,
                images: vec![],
            },
        );

        // TODO compute this off-thread?
        let max_depth = self
            .image
            .data
            .iter()
            .flat_map(|i| i.pixels.iter())
            .map(|p| p.depth)
            .max()
            .unwrap_or(0)
            .max(1);
        for image in self.image.data.iter() {
            let data = gr.shaded.get_data(device, texture_size);

            // Upload SSAO texture data
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &data.ssao_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                image.ssao.as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * width),
                    rows_per_image: Some(height),
                },
                texture_size,
            );
            // Upload geometry pixel texture data
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &data.pixel_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                image.pixels.as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(16 * width),
                    rows_per_image: Some(height),
                },
                texture_size,
            );
            let has_color = if let Some(color) = &image.color {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &data.color_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    color.as_bytes(),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * width),
                        rows_per_image: Some(height),
                    },
                    texture_size,
                );
                true
            } else {
                false
            };

            // Create the uniform
            let uniforms = Uniforms {
                transform: transform.into(),
                max_depth,
                has_color: u32::from(has_color),
                _padding: Default::default(),
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

            gr.shaded
                .bound_data
                .get_mut(&self.index)
                .unwrap()
                .images
                .push(data);
        }

        // Do deferred painting (with depth buffer) in a separate render pass
        let data = &gr.shaded.bound_data[&self.index];
        let mut render_pass = data.blit.begin_render_pass(egui_encoder);
        gr.shaded.paint(&mut render_pass, self.index);

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let rs: &WgpuResources = resources.get().unwrap();
        let data = &rs.shaded.bound_data[&self.index];

        rs.clear.paint(render_pass);
        rs.blit.paint(render_pass, &data.blit);
    }
}

/// Resources for drawing shaded images with depth culling
///
/// This is tricky, because the default `egui` render pass doesn't include a
/// depth texture.  We work around this by building a **separate** render pass
/// with a depth buffer, rendering to an RGB texture.  The `egui_wgpu` painter
/// callback then just blits this texture to the screen.
pub(crate) struct ShadedResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bound_data: HashMap<BlockIndex, ShadedBundleData>,
}

impl ShadedResources {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shaded shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/shaders/shaded.wgsl"
                    ))
                    .into(),
                ),
            });

        // Create bind group layout
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shaded bind group layout"),
                entries: &[
                    // SSAO texture and sampler
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
                    // GeometryPixel texture and sampler
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
                    // Color texture and sampler
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
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
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                    // Uniforms
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
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
                label: Some("shaded render pipeline layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        // Create the shaded render pipeline
        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("shaded render pipeline"),
                layout: Some(&pipeline_layout),
                cache: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Bgra8Unorm,
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
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::Less,
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: 1,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview: None,
            });

        Self {
            pipeline,
            bind_group_layout,
            bound_data: HashMap::new(),
        }
    }

    pub fn reset(&mut self) {
        self.bound_data.clear();
    }

    fn get_data(
        &mut self,
        device: &wgpu::Device,
        size: wgpu::Extent3d,
    ) -> ShadedData {
        // Create the texture
        let ssao_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shaded ssao texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Create the texture view
        let ssao_texture_view = ssao_texture.create_view(&Default::default());

        // Create samplers
        let ssao_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shaded ssao sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Create the texture
        let pixel_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shaded pixel texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let pixel_texture_view = pixel_texture.create_view(&Default::default());
        let pixel_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shaded pixel sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let color_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shaded color texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let color_texture_view = color_texture.create_view(&Default::default());
        let color_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shaded color sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Create the buffer
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniform buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            mapped_at_creation: false,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shaded bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &ssao_texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&ssao_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(
                        &pixel_texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&pixel_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(
                        &color_texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&color_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });

        ShadedData {
            ssao_texture,
            pixel_texture,
            color_texture,
            bind_group,
            uniform_buffer,
        }
    }

    pub fn paint(&self, render_pass: &mut wgpu::RenderPass, index: BlockIndex) {
        render_pass.set_pipeline(&self.pipeline);
        for b in &self.bound_data[&index].images {
            render_pass.set_bind_group(0, &b.bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }
    }
}

/// Resources used to render a view of shaded images
struct ShadedBundleData {
    blit: BlitData,
    images: Vec<ShadedData>,
}

/// Resources used to render a single shaded image
struct ShadedData {
    /// SSAO texture for shading
    ssao_texture: wgpu::Texture,

    /// GeometryPixel texture to render
    pixel_texture: wgpu::Texture,

    /// `Rgba8Unorm` texture to render
    color_texture: wgpu::Texture,

    /// Uniform buffer
    ///
    /// The transform matrix is common to all images, but colors can vary
    uniform_buffer: wgpu::Buffer,

    /// Bind group for shaded rendering
    bind_group: wgpu::BindGroup,
}
