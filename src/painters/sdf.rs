use super::WgpuResources;

/// Painter drawing SDFs
use crate::{view::SdfViewImage, world::BlockIndex};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use std::collections::HashMap;
use zerocopy::IntoBytes;

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
struct Uniforms {
    transform: [[f32; 4]; 4],
    min_distance: f32,
    max_distance: f32,
    has_color: u32,
    any_color: u32,
}

/// GPU callback
pub struct WgpuSdfPainter {
    /// Current view, which may differ from the image's view
    view: fidget::render::View2,
    size: fidget::render::ImageSize,

    /// Index of the block being rendered
    index: BlockIndex,

    /// Image(s) to draw to the screen
    image: SdfViewImage,
}

impl WgpuSdfPainter {
    /// Builds a new heightmap painter
    ///
    /// Note that `size` and `view` are associated with the current rendering
    /// quad; the `image` contains its own size and view transforms.
    pub fn new(
        index: BlockIndex,
        image: SdfViewImage,
        size: fidget::render::ImageSize,
        view: fidget::render::View2,
    ) -> Self {
        Self {
            index,
            image,
            size,
            view,
        }
    }
}

pub(crate) struct SdfResources {
    deferred_pipeline: wgpu::RenderPipeline,
    deferred_bind_group_layout: wgpu::BindGroupLayout,

    paint_pipeline: wgpu::RenderPipeline,
    paint_bind_group_layout: wgpu::BindGroupLayout,

    bound_data: HashMap<BlockIndex, SdfBundleData>,
}

impl SdfResources {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        // Create SDF shader module
        let shader =
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
        let vert_shader = super::vert_shader(device);

        // Create bind group layout
        let deferred_bind_group_layout =
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
        let deferred_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("sdf deferred render pipeline layout"),
                bind_group_layouts: &[&deferred_bind_group_layout],
                push_constant_ranges: &[],
            });

        // Create the SDF render pipeline
        let deferred_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("sdf render pipeline"),
                layout: Some(&deferred_pipeline_layout),
                cache: None,
                vertex: wgpu::VertexState {
                    module: &vert_shader,
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
                    depth_compare: wgpu::CompareFunction::LessEqual,
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

        let paint_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("heightmap shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/shaders/blit.wgsl"
                    ))
                    .into(),
                ),
            });

        // Create bind group layout
        let paint_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("heightmap bind group painter layout"),
                entries: &[
                    // Texture
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
                    // Sampler
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                ],
            });

        // Create render pipeline layouts
        let paint_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("heightmap render painter pipeline layout"),
                bind_group_layouts: &[&paint_bind_group_layout],
                push_constant_ranges: &[],
            });

        // Create the heightmap render pipeline
        let paint_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("heightmap render painter pipeline"),
                layout: Some(&paint_pipeline_layout),
                cache: None,
                vertex: wgpu::VertexState {
                    module: &vert_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &paint_shader,
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
                multiview: None,
            });

        Self {
            deferred_pipeline,
            deferred_bind_group_layout,
            paint_pipeline,
            paint_bind_group_layout,
            bound_data: HashMap::new(),
        }
    }

    pub fn reset(&mut self) {
        self.bound_data.clear();
    }

    /// Prepare deferred textures for a separate render pass
    fn prepare_deferred_textures(
        &mut self,
        device: &wgpu::Device,
        index: BlockIndex,
        size: wgpu::Extent3d,
    ) {
        let desc = wgpu::TextureDescriptor {
            label: Some("heightmap rendering depth texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        let depth_texture = device.create_texture(&desc);
        let depth_view = depth_texture.create_view(&Default::default());

        let rgba_desc = wgpu::TextureDescriptor {
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            label: Some("heightmap rendering rgba texture"),
            view_formats: &[],
        };
        let rgba_texture = device.create_texture(&rgba_desc);
        let rgba_view = rgba_texture.create_view(&Default::default());

        let rgba_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("heightmap rgba sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let paint_bind_group =
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("heightmap painter bind group"),
                layout: &self.paint_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &rgba_view,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&rgba_sampler),
                    },
                ],
            });

        self.bound_data.insert(
            index,
            SdfBundleData {
                depth_texture,
                depth_view,
                rgba_texture,
                rgba_view,
                paint_bind_group,
                images: vec![],
            },
        );
    }

    fn get_data(
        &mut self,
        device: &wgpu::Device,
        size: wgpu::Extent3d,
    ) -> SdfData {
        let distance_texture =
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
            });
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
                mipmap_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });

        let color_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sdf color texture"),
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
            label: Some("sdf color sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniform buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            mapped_at_creation: false,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sdf deferred bind group"),
            layout: &self.deferred_bind_group_layout,
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
            distance_texture,
            color_texture,
            bind_group,
            uniform_buffer,
        }
    }

    pub fn paint_deferred(
        &self,
        render_pass: &mut wgpu::RenderPass,
        index: BlockIndex,
    ) {
        render_pass.set_pipeline(&self.deferred_pipeline);
        for b in &self.bound_data[&index].images {
            render_pass.set_bind_group(0, &b.bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }
    }

    pub fn paint_direct(
        &self,
        render_pass: &mut wgpu::RenderPass,
        index: BlockIndex,
    ) {
        render_pass.set_pipeline(&self.paint_pipeline);
        render_pass.set_bind_group(
            0,
            &self.bound_data[&index].paint_bind_group,
            &[],
        );
        render_pass.draw(0..6, 0..1);
    }
}

/// Resources used to render a view of SDF images
struct SdfBundleData {
    #[expect(unused, reason = "used as a render target")]
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,

    #[expect(unused, reason = "used as a render target")]
    rgba_texture: wgpu::Texture,
    rgba_view: wgpu::TextureView,

    paint_bind_group: wgpu::BindGroup,

    images: Vec<SdfData>,
}

/// Resources used to render a single SDF
struct SdfData {
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
        let transform = super::transform2(
            self.image.view,
            self.image.size,
            self.view,
            self.size,
        );

        // TODO compute this off-thread?
        let mut min_distance = f32::INFINITY;
        let mut max_distance = -f32::INFINITY;
        for d in self.image.data.iter().flat_map(|i| i.distance.iter()) {
            max_distance = max_distance.max(*d);
            min_distance = min_distance.min(*d);
        }
        let any_color = self.image.data.iter().any(|i| i.color.is_some());

        gr.sdf
            .prepare_deferred_textures(device, self.index, texture_size);
        for image in self.image.data.iter() {
            let data = gr.sdf.get_data(device, texture_size);

            // Upload SDF image data
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &data.distance_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                image.distance.as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * width),
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

            let uniforms = Uniforms {
                transform: transform.into(),
                has_color: u32::from(has_color),
                any_color: u32::from(any_color),
                min_distance,
                max_distance,
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

            gr.sdf
                .bound_data
                .get_mut(&self.index)
                .unwrap()
                .images
                .push(data);
        }

        // Do deferred painting (with depth buffer) in a separate render pass
        let data = &gr.sdf.bound_data[&self.index];
        let mut render_pass =
            egui_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sdf deferred render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &data.rgba_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.1,
                            b: 0.1,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(
                    wgpu::RenderPassDepthStencilAttachment {
                        view: &data.depth_view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    },
                ),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        gr.sdf.paint_deferred(&mut render_pass, self.index);

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let rs: &WgpuResources = resources.get().unwrap();

        rs.clear.paint(render_pass);
        rs.sdf.paint_direct(render_pass, self.index);
    }
}
