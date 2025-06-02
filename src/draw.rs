//! GPU-based image drawing in an `egui` context
use crate::{
    view::{RenderMode, ViewImage},
    world::BlockIndex,
};
use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use std::collections::HashMap;
use zerocopy::IntoBytes;

/// Equivalent to the `struct Uniforms` in the WebGPU shader
#[repr(C)]
#[derive(Copy, Clone, zerocopy::IntoBytes, zerocopy::Immutable)]
pub struct Uniforms {
    pub transform: [[f32; 4]; 4],
}

/// Universal basic GPU resources
///
/// This is constructed *once* and used for every GPU rendering task in the
/// GUI.
pub struct WgpuResources {
    rgba_pipeline: wgpu::RenderPipeline,
    rgba_bind_group_layout: wgpu::BindGroupLayout,

    clear_pipeline: wgpu::RenderPipeline,
    clear_bind_group_layout: wgpu::BindGroupLayout,

    spare_bitmaps: HashMap<wgpu::Extent3d, Vec<BitmapResources>>,
    bound_bitmaps: HashMap<BlockIndex, BitmapResources>,
}

impl WgpuResources {
    pub fn reset(&mut self) {
        // Only keep around bitmaps which were bound for the last render
        self.spare_bitmaps.clear();
        for (_k, b) in std::mem::take(&mut self.bound_bitmaps) {
            self.spare_bitmaps
                .entry(b.rgba_texture.size())
                .or_default()
                .push(b);
            self.spare_bitmaps.retain(|_k, v| !v.is_empty());
        }
    }

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

        // Create bind group layout
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

        let clear_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("RGBA Shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/shaders/clear.wgsl"
                    ))
                    .into(),
                ),
            });

        // Create bind group layout
        let clear_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Clear Bind Group Layout"),
                entries: &[],
            });

        // Create render pipeline layouts
        let clear_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Clear Render Pipeline Layout"),
                bind_group_layouts: &[&clear_bind_group_layout],
                push_constant_ranges: &[],
            });

        // Create the RGBA render pipeline
        let clear_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("RGBA Render Pipeline"),
                layout: Some(&clear_pipeline_layout),
                cache: None,
                vertex: wgpu::VertexState {
                    module: &clear_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &clear_shader,
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
            clear_pipeline,
            clear_bind_group_layout,

            spare_bitmaps: HashMap::new(),
            bound_bitmaps: HashMap::new(),
        }
    }

    fn get_bitmap_resources(
        &mut self,
        device: &wgpu::Device,
        index: BlockIndex,
        size: wgpu::Extent3d,
    ) -> (&wgpu::Texture, &wgpu::Buffer) {
        let r = self
            .spare_bitmaps
            .get_mut(&size)
            .and_then(|v| v.pop())
            .unwrap_or_else(|| {
                // Create the texture
                let rgba_texture =
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("RGBA Texture"),
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
                let texture_view = rgba_texture
                    .create_view(&wgpu::TextureViewDescriptor::default());

                // Create samplers
                let rgba_sampler =
                    device.create_sampler(&wgpu::SamplerDescriptor {
                        label: Some("RGBA Sampler"),
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
                        label: Some("Uniform Buffer"),
                        size: std::mem::size_of::<Uniforms>() as u64,
                        mapped_at_creation: false,
                        usage: wgpu::BufferUsages::UNIFORM
                            | wgpu::BufferUsages::COPY_DST,
                    });

                let rgba_bind_group =
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("RGBA Bind Group"),
                        layout: &self.rgba_bind_group_layout,
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
                                    &rgba_sampler,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: uniform_buffer.as_entire_binding(),
                            },
                        ],
                    });

                let clear_bind_group =
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Clear Bind Group"),
                        layout: &self.clear_bind_group_layout,
                        entries: &[],
                    });
                BitmapResources {
                    rgba_sampler,
                    rgba_texture,
                    rgba_bind_group,
                    clear_bind_group,
                    uniform_buffer,
                }
            });
        let prev = self.bound_bitmaps.insert(index, r);
        assert!(prev.is_none());
        let r = &self.bound_bitmaps[&index];
        (&r.rgba_texture, &r.uniform_buffer)
    }
}

/// GPU callback
pub(crate) struct WgpuBitmapPainter {
    /// Current view, which may differ from the image's view
    view: fidget::render::View2,
    size: fidget::render::ImageSize,

    /// Index of the block being rendered
    index: BlockIndex,

    /// Image to render
    image: ViewImage,
}

impl WgpuBitmapPainter {
    pub fn new(
        index: BlockIndex,
        image: ViewImage,
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

struct BitmapResources {
    #[expect(unused)] // kept alive for lifetime purposes
    rgba_sampler: wgpu::Sampler,

    /// Texture to render
    rgba_texture: wgpu::Texture,

    /// Uniform buffer
    uniform_buffer: wgpu::Buffer,

    /// Bind group for RGBA rendering
    rgba_bind_group: wgpu::BindGroup,

    /// Bind group for the clear pass
    clear_bind_group: wgpu::BindGroup,
}

impl egui_wgpu::CallbackTrait for WgpuBitmapPainter {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let gr: &mut WgpuResources = resources.get_mut().unwrap();

        let (width, height) = match self.image.settings.mode {
            RenderMode::SdfApprox(s)
            | RenderMode::SdfExact(s)
            | RenderMode::Bitfield(s) => (
                (s.size.width() / (1 << self.image.level)).max(1),
                (s.size.height() / (1 << self.image.level)).max(1),
            ),
            _ => panic!("invalid render mode for bitmap painter"),
        };
        let texture_size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let (texture, uniform_buffer) =
            gr.get_bitmap_resources(device, self.index, texture_size);

        // Upload RGBA image data
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
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

        // Create the uniform
        let transform = match self.image.settings.mode {
            RenderMode::SdfApprox(s)
            | RenderMode::SdfExact(s)
            | RenderMode::Bitfield(s) => {
                // don't blame me, I just twiddled the matrices until things
                // looked right
                let aspect_ratio = |size: fidget::render::ImageSize| {
                    let width = size.width() as f32;
                    let height = size.height() as f32;
                    if width > height {
                        nalgebra::Scale2::new(height / width, 1.0)
                    } else {
                        nalgebra::Scale2::new(1.0, width / height)
                    }
                };
                let prev_aspect_ratio = aspect_ratio(s.size);
                let curr_aspect_ratio = aspect_ratio(self.size);
                let m =
                    prev_aspect_ratio.to_homogeneous().try_inverse().unwrap()
                        * curr_aspect_ratio.to_homogeneous()
                        * self.view.world_to_model().try_inverse().unwrap()
                        * s.view.world_to_model();
                #[rustfmt::skip]
                let transform = nalgebra::Matrix4::new(
                    m[(0, 0)], m[(0, 1)], 0.0, m[(0, 2)] * curr_aspect_ratio.x,
                    m[(1, 0)], m[(1, 1)], 0.0, m[(1, 2)] * curr_aspect_ratio.y,
                    0.0,         0.0,         1.0, 0.0,
                    0.0,         0.0,         0.0, 1.0,
                );
                transform
            }
            RenderMode::Heightmap(_) => {
                panic!("invalid render mode for bitmap painter");
            }
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
        let bs = &rs.bound_bitmaps[&self.index];

        render_pass.set_pipeline(&rs.clear_pipeline);
        render_pass.set_bind_group(0, &bs.clear_bind_group, &[]);
        render_pass.draw(0..6, 0..1);

        render_pass.set_pipeline(&rs.rgba_pipeline);
        render_pass.set_bind_group(0, &bs.rgba_bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}

////////////////////////////////////////////////////////////////////////////////

/// GPU callback for painting heightmaps
///
/// This is identical to the `BitmapPainter` except that it takes a 3D view
pub(crate) struct WgpuHeightmapPainter {
    /// Current view, which may differ from the image's view
    view: fidget::render::View3,
    size: fidget::render::ImageSize,

    /// Index of the block being rendered
    index: BlockIndex,

    /// Image to render
    image: ViewImage,
}

impl WgpuHeightmapPainter {
    pub fn new(
        index: BlockIndex,
        image: ViewImage,
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
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let gr: &mut WgpuResources = resources.get_mut().unwrap();

        let (width, height) = match self.image.settings.mode {
            RenderMode::Heightmap(s) => (
                (s.size.width() / (1 << self.image.level)).max(1),
                (s.size.height() / (1 << self.image.level)).max(1),
            ),
            _ => panic!("invalid painter"),
        };
        let texture_size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let (texture, uniform_buffer) =
            gr.get_bitmap_resources(device, self.index, texture_size);

        // Upload RGBA image data
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
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

        // Create the uniform
        let transform = match self.image.settings.mode {
            RenderMode::Heightmap(s) => {
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
                    aspect_ratio(s.size.width(), s.size.height());
                let curr_aspect_ratio =
                    aspect_ratio(self.size.width(), self.size.height());
                let m =
                    prev_aspect_ratio.to_homogeneous().try_inverse().unwrap()
                        * curr_aspect_ratio.to_homogeneous()
                        * self.view.world_to_model().try_inverse().unwrap()
                        * s.view.world_to_model();
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
        let bs = &rs.bound_bitmaps[&self.index];

        render_pass.set_pipeline(&rs.clear_pipeline);
        render_pass.set_bind_group(0, &bs.clear_bind_group, &[]);
        render_pass.draw(0..6, 0..1);

        render_pass.set_pipeline(&rs.rgba_pipeline);
        render_pass.set_bind_group(0, &bs.rgba_bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}
