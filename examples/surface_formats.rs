//! 診斷：列出實際視窗 surface 支援的所有格式與後端。
//!
//! 想知道「這台機器能不能輸出 HDR」時跑這個。wgpu 22 的 Vulkan 後端在
//! `config.format == Rgba16Float` 時會把 swapchain 的色彩空間設成
//! `EXTENDED_SRGB_LINEAR_EXT`（也就是 scRGB 線性，HDR 要的那個），否則用
//! `SRGB_NONLINEAR`。所以只要 Rgba16Float 出現在支援清單裡，這條路就是通的。
//!
//! egui-wgpu 自己的 `preferred_framebuffer_format` 只挑 8-bit 格式，
//! 這支程式繞過它直接問驅動。
//!
//! ```text
//! cargo run --release --example surface_formats
//! ```
//! 開一個小視窗、印出結果、隨即關閉。

use eframe::wgpu;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Default)]
struct Probe {
    done: bool,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.done {
            return;
        }
        self.done = true;

        let attrs = Window::default_attributes()
            .with_title("surface probe")
            .with_visible(false);
        let window = event_loop.create_window(attrs).expect("建立視窗");

        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(&window).expect("建立 surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("取得 adapter");

        let info = adapter.get_info();
        println!("後端      {:?}", info.backend);
        println!("裝置      {}", info.name);

        let caps = surface.get_capabilities(&adapter);
        println!("\n支援的 surface 格式（依驅動回報的順序）：");
        for f in &caps.formats {
            // wgpu-hal 的 Vulkan 後端只有在格式是 Rgba16Float 時才會把色彩空間
            // 設成 EXTENDED_SRGB_LINEAR_EXT，其餘一律 SRGB_NONLINEAR
            let note = if *f == wgpu::TextureFormat::Rgba16Float {
                "  ← scRGB 線性，HDR 輸出用的就是這個"
            } else {
                ""
            };
            println!("  {f:?}{note}");
        }

        let hdr = caps.formats.contains(&wgpu::TextureFormat::Rgba16Float);
        println!(
            "\nHDR surface：{}",
            if hdr {
                "可用"
            } else {
                "不可用（驅動沒有回報 Rgba16Float）"
            }
        );
        println!(
            "egui-wgpu 實際會挑：{:?}",
            eframe::egui_wgpu::preferred_framebuffer_format(&caps.formats).expect("有格式可用")
        );

        event_loop.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn main() {
    let event_loop = EventLoop::new().expect("建立事件迴圈");
    event_loop
        .run_app(&mut Probe::default())
        .expect("執行事件迴圈");
}
