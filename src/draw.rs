use crate::view::ViewImage;
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use zerocopy::IntoBytes;

/// Universal basic GPU resources
///
/// This is constructed *once* and used for every GPU rendering task in the
/// GUI.
pub struct WgpuResources {
    rgba_pipeline: wgpu::RenderPipeline,
    rgba_bind_group_layout: wgpu::BindGroupLayout,
}

impl WgpuResources {
    /// Installs an instance of `WgpuResources` into the callback resources
    pub fn install(wgpu_state: &eframe::egui_wgpu::RenderState) {
        let resources = Self::new(&wgpu_state.device, wgpu_state.target_format);
        wgpu_state
            .renderer
            .write()
            .callback_resources
            .insert(resources);
    }

    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        // Create RGBA shader module
        let rgba_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("RGBA Shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/shaders/image.wgsl"
                    ))
                    .into(),
                ),
            });

        // Create bind group layout (currently empty)
        let rgba_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("RGBA Bind Group Layout"),
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
        let rgba_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("RGBA Render Pipeline Layout"),
                bind_group_layouts: &[&rgba_bind_group_layout],
                push_constant_ranges: &[],
            });

        // Create the RGBA render pipeline
        let rgba_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("RGBA Render Pipeline"),
                layout: Some(&rgba_pipeline_layout),
                cache: None,
                vertex: wgpu::VertexState {
                    module: &rgba_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &rgba_shader,
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
        WgpuResources {
            rgba_pipeline,
            rgba_bind_group_layout,
        }
    }
}

/// GPU callback
pub(crate) struct WgpuPainter {
    image: ViewImage,
}

impl WgpuPainter {
    pub fn new(image: ViewImage) -> Self {
        Self { image }
    }
}

struct WgpuPainterResources {
    #[expect(unused)] // kept alive for lifetime purposes
    rgba_sampler: wgpu::Sampler,
    #[expect(unused)] // kept alive for lifetime purposes
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl egui_wgpu::CallbackTrait for WgpuPainter {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Borrow global resources
        let gr: &mut WgpuResources = resources.get_mut().unwrap();

        let (width, height) = (
            self.image.settings.size.width(),
            self.image.settings.size.height(),
        );
        let texture_size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        // XXX allocating a texture on every frame seems expensive!

        // Create the texture
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("RGBA Texture"),
            size: texture_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Upload RGBA image data
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            self.image.data.as_bytes(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            texture_size,
        );

        // Create the texture view
        let texture_view =
            texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Create samplers
        let rgba_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("RGBA Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RGBA Bind Group"),
            layout: &gr.rgba_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&rgba_sampler),
                },
            ],
        });

        resources.insert(WgpuPainterResources {
            texture,
            rgba_sampler,
            bind_group,
        });

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let rs: &WgpuResources = resources.get().unwrap();
        let bs: &WgpuPainterResources = resources.get().unwrap();

        render_pass.set_pipeline(&rs.rgba_pipeline);
        render_pass.set_bind_group(0, &bs.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}
