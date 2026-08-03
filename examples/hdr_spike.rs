//! HDR 輸出可行性驗證（spike）。**不碰主程式，只是一支獨立的測試視窗。**
//!
//! # 這支程式要回答什麼
//!
//! 靜態讀原始碼與微軟文件推導出「wgpu 22 只要把 surface 設成 `Rgba16Float`
//! 就會拿到 scRGB 線性的 HDR swapchain」：
//!
//! * Vulkan 後端會明確設 `EXTENDED_SRGB_LINEAR_EXT`
//!   （wgpu-hal-22.0.0/src/vulkan/device.rs:544）
//! * DX12 後端不設色彩空間，但 DXGI 對浮點 swapchain 的**預設**就是
//!   `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709`，也就是 scRGB
//!   （只有 HDR10／`Rgb10a2Unorm` 才硬性需要 `SetColorSpace1`）
//!
//! 推導再漂亮也還是推導。真正要付出的代價是 vendor 一份 egui-wgpu 並永久維護，
//! 所以先花很小的成本把前提驗證掉：這支程式繞過 egui，直接建 surface。
//!
//! # 怎麼判讀
//!
//! 畫面上半是階梯（scRGB 0.125 到 5.66，每階差半格光圈），下半是固定 1.0。
//! 按 **空白鍵**在兩種 surface 格式之間切換，看標題列確認目前模式。
//!
//! * **SDR（`Bgra8Unorm`）**：1.0 就是螢幕白，所以 1.0 以上的階梯全部一樣白，
//!   右半邊會是一整片沒有層次的白。
//! * **HDR（`Rgba16Float`／scRGB）**：1.0 的定義是 **80 nits**，比桌面的 SDR
//!   白階（一般 200 nits 上下）暗，所以切過去的瞬間整體應該會**變暗**——
//!   這件事本身就證明了 swapchain 真的被當成 scRGB 在處理。接著階梯應該一路
//!   繼續變亮、越過原本 SDR 會截斷的位置仍有層次。
//!
//! 如果切換前後亮度完全沒變、或右半邊在兩種模式下同樣死白，代表這條路在這台
//! 機器上不成立，整個 HDR 輸出計畫就此打住。
//!
//! ```text
//! cargo run --release --example hdr_spike
//! ```

use std::sync::Arc;

use eframe::wgpu;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const SDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

const SHADER: &str = r#"
struct Params { size: vec4<f32> };
@group(0) @binding(0) var<uniform> p: Params;

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    // 覆蓋整個畫面的三角形，不需要頂點緩衝
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) & 1) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

// 上半：12 階，exp2(i/2 - 3) → 0.125 … 5.657，第 6 階剛好是 1.0
// 下半：固定 1.0，當作對照
fn wedge(pos: vec2<f32>) -> f32 {
    if (pos.y > p.size.y * 0.5) {
        return 1.0;
    }
    let i = floor(clamp(pos.x / p.size.x, 0.0, 0.999) * 12.0);
    return exp2(i * 0.5 - 3.0);
}

// scRGB 線性：直接把值寫進去，超過 1.0 的部分就是 HDR 高光
@fragment
fn fs_linear(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let v = wedge(pos.xy);
    return vec4<f32>(v, v, v, 1.0);
}

fn srgb_encode(x: f32) -> f32 {
    let c = clamp(x, 0.0, 1.0);
    if (c <= 0.0031308) { return 12.92 * c; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

// 一般 SDR：截斷到 0–1 再做 sRGB 編碼
@fragment
fn fs_gamma(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let v = srgb_encode(wedge(pos.xy));
    return vec4<f32>(v, v, v, 1.0);
}
"#;

struct Gpu {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    pipeline: wgpu::RenderPipeline,
    hdr: bool,
    hdr_available: bool,
}

impl Gpu {
    fn pipeline_for(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::PipelineLayout,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        // scRGB surface 要寫線性值；一般 SDR surface 要寫 gamma 編碼後的值
        let entry = if format == HDR_FORMAT {
            "fs_linear"
        } else {
            "fs_gamma"
        };
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wedge"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: entry,
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
    }

    fn new(window: Arc<Window>) -> Self {
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .expect("建立 surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("取得 adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("hdr spike"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
            },
            None,
        ))
        .expect("建立裝置");

        let info = adapter.get_info();
        let caps = surface.get_capabilities(&adapter);
        let hdr_available = caps.formats.contains(&HDR_FORMAT);
        println!("後端          {:?}", info.backend);
        println!("裝置          {}", info.name);
        println!("支援的格式    {:?}", caps.formats);
        println!(
            "Rgba16Float   {}",
            if hdr_available {
                "有——scRGB 線性 HDR surface 可用"
            } else {
                "無——這台機器走不了 HDR 輸出"
            }
        );

        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: SDR_FORMAT,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wedge"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = Self::pipeline_for(&device, &shader, &layout, SDR_FORMAT);

        let me = Self {
            window,
            surface,
            device,
            queue,
            config,
            shader,
            layout,
            bind_group,
            uniform,
            pipeline,
            hdr: false,
            hdr_available,
        };
        me.retitle();
        me
    }

    fn retitle(&self) {
        self.window.set_title(&format!(
            "HDR spike — {} （空白鍵切換）",
            if self.hdr {
                "Rgba16Float / scRGB 線性"
            } else {
                "Bgra8Unorm / SDR"
            }
        ));
    }

    fn toggle(&mut self) {
        if !self.hdr_available {
            println!("這台機器沒有 Rgba16Float surface，無法切換");
            return;
        }
        self.hdr = !self.hdr;
        self.config.format = if self.hdr { HDR_FORMAT } else { SDR_FORMAT };
        self.surface.configure(&self.device, &self.config);
        self.pipeline =
            Self::pipeline_for(&self.device, &self.shader, &self.layout, self.config.format);
        self.retitle();
        println!("切換到 {:?}", self.config.format);
        self.window.request_redraw();
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    fn draw(&mut self) {
        self.queue.write_buffer(
            &self.uniform,
            0,
            bytemuck_pod(&[
                self.config.width as f32,
                self.config.height as f32,
                0.0,
                0.0,
            ]),
        );
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        frame.present();
    }
}

/// 把 f32 陣列當成位元組看待。只在這支診斷程式裡用，不值得為它拉一個相依套件。
fn bytemuck_pod(v: &[f32; 4]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

#[derive(Default)]
struct App {
    gpu: Option<Gpu>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("HDR spike")
            .with_inner_size(winit::dpi::LogicalSize::new(960.0, 480.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("建立視窗"));
        self.gpu = Some(Gpu::new(window));
        println!(
            "\n上半是階梯（scRGB 0.125 … 5.66），下半固定 1.0 當對照。\n\
             空白鍵切換 surface 格式，Esc 離開。\n\n\
             預期：切到 Rgba16Float 時整體會先變暗（scRGB 的 1.0 只有 80 nits，\n\
             比桌面 SDR 白階低），但階梯右半會一路繼續變亮而不像 SDR 那樣死白。\n\
             若切換前後看起來完全一樣，代表這條路不成立。"
        );
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                gpu.resize(size.width, size.height);
                gpu.window.request_redraw();
            }
            WindowEvent::RedrawRequested => gpu.draw(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match logical_key {
                Key::Named(NamedKey::Space) => gpu.toggle(),
                Key::Named(NamedKey::Escape) => event_loop.exit(),
                _ => {}
            },
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("建立事件迴圈");
    event_loop
        .run_app(&mut App::default())
        .expect("執行事件迴圈");
}
