use super::WgpuResources;
use crate::{render::VoxelRenderSettings, view::TextureData};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use zerocopy::IntoBytes;

/// GPU callback for painting shaded objects
pub struct WgpuRgbaPainter {
    /// Current view, which may differ from the image's view
    view: fidget::gui::View3,
    size: fidget::render::ImageSize,

    /// Texture to render
    data: TextureData<u32>,

    /// Buffer sized for a [`Uniforms`] object
    uniforms: wgpu::Buffer,

    /// Bind group to use when rendering
    ///
    /// This will include the texture from `data` (which we still need because
    /// we may be responsible for queueing the CPU -> GPU copy) and the buffer
    /// from `uniforms` (likewise)..  It also includes a texture view, which
    /// doesn't need to be explicitly stored here (because we have nothing to do
    /// with it).
    bind_group: wgpu::BindGroup,

    /// Settings with which the image was rendered
    settings: VoxelRenderSettings,
}

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
pub struct Uniforms {
    transform: [[f32; 4]; 4],
}

impl WgpuRgbaPainter {
    /// Builds a new shaded painter
    ///
    /// Note that `size` and `view` are associated with the current rendering
    /// quad; the `settings` contains the image's render settings
    pub fn new(
        data: TextureData<u32>,
        uniforms: wgpu::Buffer,
        bind_group: wgpu::BindGroup,
        settings: VoxelRenderSettings,
        size: fidget::render::ImageSize,
        view: fidget::gui::View3,
    ) -> Self {
        Self {
            data,
            uniforms,
            bind_group,
            settings,
            size,
            view,
        }
    }
}

impl egui_wgpu::CallbackTrait for WgpuRgbaPainter {
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

        let transform = super::transform3(
            self.settings.view,
            render_size,
            self.view,
            self.size,
        );

        // Write the image texture (if not already done)
        if !self.data.data.is_empty() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.data.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                self.data.data.as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * texture_size.width),
                    rows_per_image: Some(texture_size.height),
                },
                texture_size,
            );
        }

        // Write the uniforms
        let uniforms = Uniforms {
            transform: transform.into(),
        };
        let mut writer = queue
            .write_buffer_with(
                &self.uniforms,
                0,
                (std::mem::size_of_val(&uniforms) as u64)
                    .try_into()
                    .unwrap(),
            )
            .unwrap();
        writer.copy_from_slice(uniforms.as_bytes());

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

        render_pass.set_pipeline(&rs.rgba.pipeline);
        render_pass.set_bind_group(0, Some(&self.bind_group), &[]);
        render_pass.draw(0..6, 0..1);
    }
}

/// Resources for drawing RGBA images
///
/// There is a single copy of this resources object, and it's available during
/// both preparation and painting passes.
#[derive(Clone)]
pub(crate) struct RgbaResources {
    pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub rgba_sampler: wgpu::Sampler,
}

impl RgbaResources {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shaded shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/shaders/rgba.wgsl"
                    ))
                    .into(),
                ),
            });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("rgba bind group layout"),
                entries: &[
                    // rgba texture
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
                    // rgba sampler
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                    // config uniform
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
                label: Some("rgba pipeline layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0u32,
            });

        // Create the shaded render pipeline
        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("rgba blit pipeline"),
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
                multiview_mask: None,
            });

        let rgba_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rgba sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        Self {
            pipeline,
            bind_group_layout,
            rgba_sampler,
        }
    }
}
