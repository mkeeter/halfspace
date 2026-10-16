use super::WgpuResources;
use crate::{
    render::VoxelRenderSettings, view::TextureData, world::BlockIndex,
};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use std::collections::HashMap;
use zerocopy::IntoBytes;

/// GPU callback for painting shaded objects
pub struct WgpuRgbaPainter {
    /// Current view, which may differ from the image's view
    view: fidget::gui::View3,
    size: fidget::render::ImageSize,

    /// Index of the block being rendered
    index: BlockIndex,

    /// Texture to render
    data: TextureData<u32>,

    /// Buffer sized for a [`Uniforms`] object
    uniforms: wgpu::Buffer,

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
    /// quad; the `image` contains its own size and view transforms.
    pub fn new(
        index: BlockIndex,
        data: TextureData<u32>,
        uniforms: wgpu::Buffer,
        settings: VoxelRenderSettings,
        size: fidget::render::ImageSize,
        view: fidget::gui::View3,
    ) -> Self {
        Self {
            index,
            data,
            uniforms,
            settings,
            size,
            view,
        }
    }
}

impl egui_wgpu::CallbackTrait for WgpuRgbaPainter {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let gr: &mut WgpuResources = resources.get_mut().unwrap();

        let image_size = self.settings.size;
        let width = (image_size.width() / (1 << self.settings.level)).max(1);
        let height = (image_size.height() / (1 << self.settings.level)).max(1);
        let texture_size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let transform = super::transform3(
            self.settings.view,
            self.settings.size,
            self.view,
            self.size,
        );

        // Write the image texture
        let data = gr.rgba.get_data(
            &self.data,
            &self.uniforms,
            device,
            queue,
            texture_size,
        );

        let uniforms = Uniforms {
            transform: transform.into(),
        };
        {
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
        }

        let prev = gr.rgba.bound_data.insert(self.index, data);
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
        rs.clear.paint(render_pass);
        rs.rgba.paint(render_pass, self.index);
    }
}

/// Resources for drawing RGBA images
///
/// There is a single copy of this resources object, and it's available during
/// both preparation and painting passes.
pub(crate) struct RgbaResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,

    /// Per-frame bound data
    ///
    /// This is cleared at the start of the frame, populated in
    /// [`WgpuRgbaPainter::prepare`], and used in [`RgbaResources::paint`]
    bound_data: HashMap<BlockIndex, RgbaData>,

    rgba_sampler: wgpu::Sampler,
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
            bound_data: HashMap::new(),
            rgba_sampler,
        }
    }

    pub fn reset(&mut self) {
        self.bound_data.clear();
    }

    fn get_data(
        &mut self,
        data: &TextureData<u32>,
        uniform_buffer: &wgpu::Buffer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: wgpu::Extent3d,
    ) -> RgbaData {
        let rgba_texture_view = data.texture.create_view(&Default::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rgba bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &rgba_texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(
                        &self.rgba_sampler,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        if !data.data.is_empty() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &data.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data.data.as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * size.width),
                    rows_per_image: Some(size.height),
                },
                size,
            );
        }

        RgbaData { bind_group }
    }

    pub fn paint(&self, render_pass: &mut wgpu::RenderPass, index: BlockIndex) {
        render_pass.set_pipeline(&self.pipeline);
        let b = &self.bound_data[&index];
        render_pass.set_bind_group(0, &b.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}

/// Resources used to render a single shaded image
struct RgbaData {
    /// Bind group for bitfield rendering
    bind_group: wgpu::BindGroup,
}
