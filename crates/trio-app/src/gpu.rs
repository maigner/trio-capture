//! wgpu compositor: three source textures in, one composed frame out.

use bytemuck::{Pod, Zeroable};

use trio_core::layout::slot_rects;
use trio_core::{Grade, Project, Slot};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
pub struct Uniforms {
    pub out_size: [f32; 4],
    pub slot_rect: [[f32; 4]; 3],
    pub slot_params: [[f32; 4]; 3],
    pub src_info: [[f32; 4]; 3],
    pub grade_a: [[f32; 4]; 3],
    pub grade_b: [[f32; 4]; 3],
}

impl Uniforms {
    pub fn from_project(
        project: &Project,
        out_w: u32,
        out_h: u32,
        src: &[Option<(u32, u32)>; 3],
    ) -> Self {
        let rects = slot_rects(project.layout);
        let mut u = Uniforms {
            out_size: [out_w as f32, out_h as f32, 0.0, 0.0],
            ..Default::default()
        };
        for (i, r) in rects.iter().enumerate() {
            u.slot_rect[i] = [r.x, r.y, r.w, r.h];
            let s: Slot = project.slots[i];
            u.slot_params[i] = [s.zoom, s.pan[0], s.pan[1], s.camera.min(2) as f32];
        }
        for i in 0..3 {
            let g: Grade = project.cameras.get(i).map(|c| c.grade).unwrap_or_default();
            u.grade_a[i] = [g.exposure, g.contrast, g.saturation, g.temperature];
            u.grade_b[i] = [g.tint, g.lift, g.gamma, g.gain];
            u.src_info[i] = match src[i] {
                Some((w, h)) => [w as f32, h as f32, 1.0, 0.0],
                None => [1.0, 1.0, 0.0, 0.0],
            };
        }
        u
    }
}

pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Non-sRGB reinterpretation of [`OUTPUT_FORMAT`] for egui, which expects
/// gamma-encoded texels from the textures it draws.
pub const DISPLAY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub struct Compositor {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl Compositor {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex_entry(2),
                tex_entry(3),
                tex_entry(4),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite"),
            layout: Some(&layout),
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
                    format: OUTPUT_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("composite"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Self {
            device,
            queue,
            pipeline,
            bind_layout,
            sampler,
        }
    }

    pub fn create_target(&self, width: u32, height: u32) -> Target {
        let output = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("composite output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[DISPLAY_FORMAT],
        });
        let view = output.create_view(&Default::default());
        // egui treats sampled texels as already gamma encoded, so it must read
        // the stored sRGB bytes untouched. Through the sRGB view it would get
        // linear values and show the preview far too dark.
        let display_view = output.create_view(&wgpu::TextureViewDescriptor {
            label: Some("composite display"),
            format: Some(DISPLAY_FORMAT),
            ..Default::default()
        });
        let uniform_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sources = [
            self.make_source(2, 2),
            self.make_source(2, 2),
            self.make_source(2, 2),
        ];
        let bind_group = self.make_bind_group(&uniform_buf, &sources);
        Target {
            width,
            height,
            output,
            view,
            display_view,
            sources,
            uniform_buf,
            bind_group,
            src_sizes: [None, None, None],
            readback: None,
        }
    }

    fn make_source(&self, w: u32, h: u32) -> SourceTex {
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("camera source"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        SourceTex { tex, view, w, h }
    }

    fn make_bind_group(
        &self,
        uniform_buf: &wgpu::Buffer,
        sources: &[SourceTex; 3],
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&sources[0].view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&sources[1].view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&sources[2].view),
                },
            ],
        })
    }

    /// Upload an RGBA frame for camera `cam`, resizing the texture if needed.
    pub fn upload(&self, target: &mut Target, cam: usize, w: u32, h: u32, rgba: &[u8]) {
        if target.sources[cam].w != w || target.sources[cam].h != h {
            target.sources[cam] = self.make_source(w, h);
            target.bind_group = self.make_bind_group(&target.uniform_buf, &target.sources);
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &target.sources[cam].tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        target.src_sizes[cam] = Some((w, h));
    }

    pub fn clear_source(&self, target: &mut Target, cam: usize) {
        target.src_sizes[cam] = None;
    }

    pub fn render(&self, target: &Target, project: &Project) {
        let uniforms =
            Uniforms::from_project(project, target.width, target.height, &target.src_sizes);
        self.queue
            .write_buffer(&target.uniform_buf, 0, bytemuck::bytes_of(&uniforms));
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("composite"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &target.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
    }

    /// Synchronously read the composed frame back as tightly packed RGBA.
    pub fn readback(&self, target: &mut Target) -> Vec<u8> {
        let bpr = target.width * 4;
        let padded =
            bpr.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let size = (padded * target.height) as u64;
        if target.readback.as_ref().map(|b| b.size()) != Some(size) {
            target.readback = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }));
        }
        let buf = target.readback.as_ref().unwrap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(target.height),
                },
            },
            wgpu::Extent3d {
                width: target.width,
                height: target.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .ok()
            .and_then(|r| r.ok())
            .expect("readback map failed");
        let mut out = Vec::with_capacity((bpr * target.height) as usize);
        {
            let data = slice.get_mapped_range();
            for row in 0..target.height as usize {
                let s = row * padded as usize;
                out.extend_from_slice(&data[s..s + bpr as usize]);
            }
        }
        buf.unmap();
        out
    }
}

pub struct SourceTex {
    pub tex: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub w: u32,
    pub h: u32,
}

pub struct Target {
    pub width: u32,
    pub height: u32,
    pub output: wgpu::Texture,
    /// Render target view (sRGB encoding on write).
    pub view: wgpu::TextureView,
    /// Same texture for egui: raw bytes, no sRGB decoding on sample.
    pub display_view: wgpu::TextureView,
    sources: [SourceTex; 3],
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    pub src_sizes: [Option<(u32, u32)>; 3],
    readback: Option<wgpu::Buffer>,
}
