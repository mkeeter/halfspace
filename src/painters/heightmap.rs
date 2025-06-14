use super::{Uniforms, WgpuResources};
use crate::{
    view::{ViewData3, ViewImage},
    world::BlockIndex,
};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use std::collections::HashMap;
use zerocopy::IntoBytes;

/// GPU callback for painting heightmaps
///
/// This is identical to the `HeightmapPainter` except that it takes a 3D view
pub struct WgpuHeightmapPainter {
    /// Current view, which may differ from the image's view
    view: fidget::render::View3,
    size: fidget::render::ImageSize,

    /// Index of the block being rendered
    index: BlockIndex,

    /// Image to render
    image: ViewImage,
}

impl WgpuHeightmapPainter {
    /// Builds a new heightmap painter
    ///
    /// Note that `size` and `view` are associated with the current rendering
    /// quad; the `image` contains its own size and view transforms.
    ///
    /// # Panics
    /// If image data is not a heightmap
    pub fn new(
        index: BlockIndex,
        image: ViewImage,
        size: fidget::render::ImageSize,
        view: fidget::render::View3,
    ) -> Self {
        assert!(matches!(
            image,
            ViewImage::View3 {
                data: ViewData3::Heightmap(..),
                ..
            }
        ));
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
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let gr: &mut WgpuResources = resources.get_mut().unwrap();

        let (width, height) = match &self.image {
            ViewImage::View3 { size, level, .. } => (
                (size.width() / (1 << level)).max(1),
                (size.height() / (1 << level)).max(1),
            ),
            _ => panic!("invalid painter"),
        };
        let texture_size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let (texture, uniform_buffer) =
            gr.heightmap.get_data(device, self.index, texture_size);

        // Upload RGBA image data
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            self.image.as_bytes(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width),
                rows_per_image: Some(height),
            },
            texture_size,
        );

        // Create the uniform
        let transform = match &self.image {
            ViewImage::View3 { size, view, .. } => {
                // don't blame me, I just twiddled the matrices until things
                // looked right
                let aspect_ratio = |width: u32, height: u32| {
                    let width = width as f32;
                    let height = height as f32;
                    if width > height {
                        nalgebra::Scale3::new(height / width, 1.0, 1.0)
                    } else {
                        nalgebra::Scale3::new(1.0, width / height, 1.0)
                    }
                };
                let prev_aspect_ratio =
                    aspect_ratio(size.width(), size.height());
                let curr_aspect_ratio =
                    aspect_ratio(self.size.width(), self.size.height());
                let m =
                    prev_aspect_ratio.to_homogeneous().try_inverse().unwrap()
                        * curr_aspect_ratio.to_homogeneous()
                        * self.view.world_to_model().try_inverse().unwrap()
                        * view.world_to_model();
                #[rustfmt::skip]
                let transform = nalgebra::Matrix4::new(
                    m[(0, 0)], m[(0, 1)], m[(0, 2)], m[(0, 3)] * curr_aspect_ratio.x,
                    m[(1, 0)], m[(1, 1)], m[(1, 2)], m[(1, 3)] * curr_aspect_ratio.y,
                    m[(2, 0)], m[(2, 1)], m[(2, 2)], m[(2, 3)],
                    0.0,         0.0,         0.0, 1.0,
                );
                transform
            }
            _ => unreachable!(),
        };
        let uniforms = Uniforms {
            transform: transform.into(),
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
        rs.heightmap.paint(render_pass, self.index);
    }
}

pub(crate) struct HeightmapResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,

    spare_data: HashMap<wgpu::Extent3d, Vec<HeightmapData>>,
    bound_data: HashMap<BlockIndex, HeightmapData>,
}

impl HeightmapResources {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        // Create heightmap shader module.  Right now, this is the same as the
        // bitmap shader, but may change in the future.
        let shader =
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
        let bind_group_layout =
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
                    // Transform matrix
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::VERTEX,
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
                label: Some("heightmap render pipeline layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        // Create the heightmap render pipeline
        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("heightmap render pipeline"),
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
            spare_data: HashMap::new(),
        }
    }

    pub fn reset(&mut self) {
        // Only keep around bitmaps which were bound for the last render
        self.spare_data.clear();
        for (_k, b) in std::mem::take(&mut self.bound_data) {
            self.spare_data.entry(b.texture.size()).or_default().push(b);
            self.spare_data.retain(|_k, v| !v.is_empty());
        }
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
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("heightmap texture"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                // Create the texture view
                let texture_view = texture
                    .create_view(&wgpu::TextureViewDescriptor::default());

                // Create samplers
                let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("heightmap sampler"),
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
                        layout: &self.bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(
                                    &texture_view,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(
                                    &sampler,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: uniform_buffer.as_entire_binding(),
                            },
                        ],
                    });

                HeightmapData {
                    sampler,
                    texture,
                    bind_group,
                    uniform_buffer,
                }
            });
        let prev = self.bound_data.insert(index, r);
        assert!(prev.is_none());
        let r = &self.bound_data[&index];
        (&r.texture, &r.uniform_buffer)
    }

    pub fn paint(&self, render_pass: &mut wgpu::RenderPass, index: BlockIndex) {
        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, &self.bound_data[&index].bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}

/// Resources used to render a single heightmap
pub(crate) struct HeightmapData {
    #[expect(unused)] // kept alive for lifetime purposes
    sampler: wgpu::Sampler,

    /// Texture to render
    texture: wgpu::Texture,

    /// Uniform buffer
    uniform_buffer: wgpu::Buffer,

    /// Bind group for heightmap rendering
    bind_group: wgpu::BindGroup,
}
