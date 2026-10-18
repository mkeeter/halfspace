//! Painter drawing SDFs and bitfields in a 2D view
use super::WgpuResources;

use crate::{
    render::Float16,
    render::ImageRenderSettings,
    view::{TextureData, ViewMode2},
};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use zerocopy::IntoBytes;

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
pub struct Uniforms {
    transform: [[f32; 4]; 4],
    has_color: u32,
    _pad: [u32; 3],
}

/// GPU callback
pub struct WgpuSdfPainter {
    /// Current view, which may differ from the image's view
    view: fidget::gui::View2,
    size: fidget::render::ImageSize,

    // GPU data
    distance: TextureData<Float16>,
    color: Option<TextureData<[u8; 4]>>,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,

    /// Settings with which the image was rendered
    settings: ImageRenderSettings,
}

impl WgpuSdfPainter {
    /// Builds a new heightmap painter
    ///
    /// Note that `size` and `view` are associated with the current rendering
    /// quad; the `image` contains its own size and view transforms.
    pub fn new(
        distance: TextureData<Float16>,
        color: Option<TextureData<[u8; 4]>>,
        uniform_buffer: wgpu::Buffer,
        bind_group: wgpu::BindGroup,
        settings: ImageRenderSettings,
        size: fidget::render::ImageSize,
        view: fidget::gui::View2,
    ) -> Self {
        Self {
            size,
            view,
            uniform_buffer,
            bind_group,
            distance,
            color,
            settings,
        }
    }
}

/// Resources for drawing SDF (2D) images
///
/// There is a single copy of this resources object, and it's available during
/// both preparation and painting passes.
#[derive(Clone)]
pub(crate) struct SdfResources {
    bitfield_pipeline: wgpu::RenderPipeline,
    sdf_pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,

    /// Empty texture used when we don't have a color channel
    pub dummy_color_texture: wgpu::Texture,

    pub distance_sampler: wgpu::Sampler,
    pub color_sampler: wgpu::Sampler,
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

        Self {
            sdf_pipeline,
            bitfield_pipeline,
            bind_group_layout,
            distance_sampler,
            color_sampler,
            dummy_color_texture,
        }
    }
}

impl egui_wgpu::CallbackTrait for WgpuSdfPainter {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        _resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let render_size = self.settings.render_size();
        let texture_size = wgpu::Extent3d {
            width: render_size.width(),
            height: render_size.height(),
            depth_or_array_layers: 1,
        };
        let transform = super::transform2(
            self.settings.view,
            render_size,
            self.view,
            self.size,
        );

        // Write the distance texture
        if !self.distance.data.is_empty() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.distance.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                self.distance.data.as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(2 * texture_size.width),
                    rows_per_image: Some(texture_size.height),
                },
                texture_size,
            );
        }

        if let Some(color) = &self.color
            && !color.data.is_empty()
        {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &color.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                color.data.as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * texture_size.width),
                    rows_per_image: Some(texture_size.height),
                },
                texture_size,
            );
        }

        let uniforms = Uniforms {
            transform: transform.into(),
            has_color: u32::from(self.color.is_some()),
            _pad: [0; _],
        };
        {
            let mut writer = queue
                .write_buffer_with(
                    &self.uniform_buffer,
                    0,
                    (std::mem::size_of_val(&uniforms) as u64)
                        .try_into()
                        .unwrap(),
                )
                .unwrap();
            writer.copy_from_slice(uniforms.as_bytes());
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
        match self.settings.mode {
            ViewMode2::Sdf => {
                render_pass.set_pipeline(&rs.sdf.sdf_pipeline);
            }
            ViewMode2::Bitfield => {
                render_pass.set_pipeline(&rs.sdf.bitfield_pipeline);
            }
        }
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}
