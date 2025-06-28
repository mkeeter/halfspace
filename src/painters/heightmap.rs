use super::WgpuResources;
use crate::{view::HeightmapViewImage, world::BlockIndex};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use std::collections::HashMap;
use zerocopy::IntoBytes;

/// GPU callback for painting heightmap objects
pub struct WgpuHeightmapPainter {
    /// Current view, which may differ from the image's view
    view: fidget::render::View3,
    size: fidget::render::ImageSize,

    /// Index of the block being rendered
    index: BlockIndex,

    /// Image(s) to draw to the screen
    image: HeightmapViewImage,
}

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
struct Uniforms {
    transform: [[f32; 4]; 4],
    color: [f32; 4],
    max_depth: f32,
    _padding: [u32; 3],
}

impl WgpuHeightmapPainter {
    /// Builds a new heightmap painter
    ///
    /// Note that `size` and `view` are associated with the current rendering
    /// quad; the `image` contains its own size and view transforms.
    pub fn new(
        index: BlockIndex,
        image: HeightmapViewImage,
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

impl egui_wgpu::CallbackTrait for WgpuHeightmapPainter {
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

        gr.heightmap.prepare_deferred_textures(
            device,
            self.index,
            texture_size,
            gr.heightmap.target_format,
        );

        // TODO compute this off-thread?
        let max_depth = self
            .image
            .data
            .iter()
            .flat_map(|i| i.data.iter())
            .map(|p| egui::emath::OrderedFloat(*p))
            .max()
            .map(|p| p.0)
            .unwrap_or(0.0)
            .max(1.0);
        for image in self.image.data.iter() {
            let (height_texture, uniform_buffer) =
                gr.heightmap.get_data(device, self.index, texture_size);

            // Upload heightmap pixel texture data
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: height_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                image.as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * width),
                    rows_per_image: Some(height),
                },
                texture_size,
            );

            // Create the uniform
            let uniforms = Uniforms {
                transform: transform.into(),
                color: image.rgba(),
                max_depth,
                _padding: [0; 3],
            };
            let mut writer = queue
                .write_buffer_with(
                    uniform_buffer,
                    0,
                    (std::mem::size_of_val(&uniforms) as u64)
                        .try_into()
                        .unwrap(),
                )
                .unwrap();
            writer.copy_from_slice(uniforms.as_bytes());
        }

        // Do deferred painting (with depth buffer) in a separate render pass
        let data = &gr.heightmap.bound_data[&self.index];
        let mut render_pass =
            egui_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("heightmap render"),
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
        gr.heightmap.paint_deferred(&mut render_pass, self.index);

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
        rs.heightmap.paint_direct(render_pass, self.index);
    }
}

/// Resources for drawing heightmap images with depth culling
///
/// This is tricky, because the default `egui` render pass doesn't include a
/// depth texture.  We work around this by building a **separate** render pass
/// with a depth buffer, rendering to an RGB texture.  The `egui_wgpu` painter
/// callback then just blits this texture to the screen.
pub(crate) struct HeightmapResources {
    deferred_pipeline: wgpu::RenderPipeline,
    deferred_bind_group_layout: wgpu::BindGroupLayout,

    paint_pipeline: wgpu::RenderPipeline,
    paint_bind_group_layout: wgpu::BindGroupLayout,

    target_format: wgpu::TextureFormat,

    spare_data: HashMap<wgpu::Extent3d, Vec<HeightmapData>>,
    spare_depth: HashMap<wgpu::Extent3d, Vec<wgpu::Texture>>,
    bound_data: HashMap<BlockIndex, HeightmapBundleData>,
}

impl HeightmapResources {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let deferred_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("heightmap shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/shaders/heightmap.wgsl"
                    ))
                    .into(),
                ),
            });

        // Create bind group layout
        let deferred_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("heightmap bind group layout"),
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
                    // Uniforms
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
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
                label: Some("heightmap render pipeline layout"),
                bind_group_layouts: &[&deferred_bind_group_layout],
                push_constant_ranges: &[],
            });

        // Create the heightmap render pipeline
        let deferred_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("heightmap render pipeline"),
                layout: Some(&deferred_pipeline_layout),
                cache: None,
                vertex: wgpu::VertexState {
                    module: &deferred_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &deferred_shader,
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
                    module: &paint_shader,
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
            target_format,
            bound_data: HashMap::new(),
            spare_depth: HashMap::new(),
            spare_data: HashMap::new(),
        }
    }

    pub fn reset(&mut self) {
        // Only keep around bitmaps which were bound for the last render
        self.spare_data.clear();
        self.spare_depth.clear();

        // Move all bound objects into spare objects
        for (_k, b) in std::mem::take(&mut self.bound_data) {
            for b in b.images {
                self.spare_data
                    .entry(b.height_texture.size())
                    .or_default()
                    .push(b);
            }
            self.spare_depth
                .entry(b.depth_texture.size())
                .or_default()
                .push(b.depth_texture);
        }
        self.spare_data.retain(|_k, v| !v.is_empty());
        self.spare_depth.retain(|_k, v| !v.is_empty());
    }

    /// Prepare deferred textures for a separate render pass
    fn prepare_deferred_textures(
        &mut self,
        device: &wgpu::Device,
        index: BlockIndex,
        size: wgpu::Extent3d,
        render_format: wgpu::TextureFormat,
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
            format: render_format,
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
            HeightmapBundleData {
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
        index: BlockIndex,
        size: wgpu::Extent3d,
    ) -> (&wgpu::Texture, &wgpu::Buffer) {
        let r = self
            .spare_data
            .get_mut(&size)
            .and_then(|v| v.pop())
            .unwrap_or_else(|| {
                // Create the texture
                let height_texture =
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("heightmap pixel texture"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::R32Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                let height_texture_view =
                    height_texture.create_view(&Default::default());
                let height_sampler =
                    device.create_sampler(&wgpu::SamplerDescriptor {
                        label: Some("heightmap pixel sampler"),
                        address_mode_u: wgpu::AddressMode::ClampToEdge,
                        address_mode_v: wgpu::AddressMode::ClampToEdge,
                        address_mode_w: wgpu::AddressMode::ClampToEdge,
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::FilterMode::Linear,
                        ..Default::default()
                    });

                // Create the buffer
                let uniform_buffer =
                    device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("uniform buffer"),
                        size: std::mem::size_of::<Uniforms>() as u64,
                        mapped_at_creation: false,
                        usage: wgpu::BufferUsages::UNIFORM
                            | wgpu::BufferUsages::COPY_DST,
                    });

                let bind_group =
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("heightmap bind group"),
                        layout: &self.deferred_bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(
                                    &height_texture_view,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(
                                    &height_sampler,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: uniform_buffer.as_entire_binding(),
                            },
                        ],
                    });

                HeightmapData {
                    height_texture,
                    bind_group,
                    uniform_buffer,
                }
            });

        self.bound_data.get_mut(&index).unwrap().images.push(r);
        let r = &self.bound_data[&index].images.last().unwrap();
        (&r.height_texture, &r.uniform_buffer)
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

/// Resources used to render a view of heightmap images
struct HeightmapBundleData {
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,

    #[expect(unused, reason = "used as a render target")]
    rgba_texture: wgpu::Texture,
    rgba_view: wgpu::TextureView,

    paint_bind_group: wgpu::BindGroup,

    images: Vec<HeightmapData>,
}

/// Resources used to render a single heightmap image
struct HeightmapData {
    /// GeometryPixel texture to render
    height_texture: wgpu::Texture,

    /// Uniform buffer
    ///
    /// The transform matrix is common to all images, but colors can vary
    uniform_buffer: wgpu::Buffer,

    /// Bind group for heightmap rendering
    bind_group: wgpu::BindGroup,
}
