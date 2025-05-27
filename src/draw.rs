use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};

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
                entries: &[],
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
pub(crate) struct WgpuPainter;

impl WgpuPainter {
    pub fn new() -> Self {
        Self
    }
}

struct WgpuPainterResources {
    bind_group: wgpu::BindGroup,
}

impl egui_wgpu::CallbackTrait for WgpuPainter {
    fn prepare(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if !resources.contains::<WgpuPainterResources>() {
            let r: &mut WgpuResources = resources.get_mut().unwrap();
            let bind_group =
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("RGBA Bind Group"),
                    layout: &r.rgba_bind_group_layout,
                    entries: &[],
                });
            resources.insert(WgpuPainterResources { bind_group });
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
        let bs: &WgpuPainterResources = resources.get().unwrap();

        render_pass.set_pipeline(&rs.rgba_pipeline);
        render_pass.set_bind_group(0, &bs.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}
