use super::WgpuResources;
use crate::{view::DebugViewImage, world::BlockIndex};
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
}

/// GPU callback for painting debug images
pub struct WgpuDebugPainter {
    /// Current view, which may differ from the image's view
    view: fidget::render::View2,
    size: fidget::render::ImageSize,

    /// Index of the block being rendered
    index: BlockIndex,

    /// Image(s) to draw to the screen
    image: DebugViewImage,
}

impl WgpuDebugPainter {
    /// Builds a new debug painter
    ///
    /// Note that `size` and `view` are associated with the current rendering
    /// quad; the `image` contains its own size and view transforms.
    pub fn new(
        index: BlockIndex,
        image: DebugViewImage,
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

impl egui_wgpu::CallbackTrait for WgpuDebugPainter {
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

        for image in self.image.data.iter() {
            let data = gr.debug.get_data(device, texture_size);

            // Upload RGBA image data
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &data.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                image.pixels.as_bytes(),
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

            gr.debug
                .bound_data
                .entry(self.index)
                .or_default()
                .push(data);
        }

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
        rs.debug.paint(render_pass, self.index);
    }
}

pub(crate) struct DebugResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,

    bound_data: HashMap<BlockIndex, Vec<DebugData>>,
}

impl DebugResources {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        // Create the debug shader module.
        let shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("debug shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/shaders/image.wgsl"
                    ))
                    .into(),
                ),
            });

        // Create bind group layout
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("debug bind group layout"),
                entries: &[
                    // Texture and sampler for color
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
        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("debug render pipeline layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        // Create the debug render pipeline
        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("debug render pipeline"),
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
    ) -> DebugData {
        // Create the texture
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("debug texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Create the texture view
        let texture_view = texture.create_view(&Default::default());

        // Create samplers
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("debug sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
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
            label: Some("debug bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });

        DebugData {
            texture,
            bind_group,
            uniform_buffer,
        }
    }

    pub fn paint(&self, render_pass: &mut wgpu::RenderPass, index: BlockIndex) {
        render_pass.set_pipeline(&self.pipeline);
        for b in &self.bound_data[&index] {
            render_pass.set_bind_group(0, &b.bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }
    }
}

/// Resources used to render a single debug image
struct DebugData {
    /// Texture to render
    texture: wgpu::Texture,

    /// Uniform buffer
    uniform_buffer: wgpu::Buffer,

    /// Bind group for debug rendering
    bind_group: wgpu::BindGroup,
}
