#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Zoetrope 走馬燈 — 極速看圖\n\n用法：zoetrope [--gl] [圖片或資料夾路徑]\n  --gl   使用 OpenGL 後端（預設為 wgpu：DX12/Vulkan/Metal）"
        );
        return Ok(());
    }
    let use_gl = args.iter().any(|a| a == "--gl");
    let path: Option<PathBuf> = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from);

    let (renderer, renderer_label) = if use_gl {
        (eframe::Renderer::Glow, "OpenGL (glow)".to_owned())
    } else {
        (
            eframe::Renderer::Wgpu,
            "wgpu (DX12 / Vulkan / Metal)".to_owned(),
        )
    };

    let options = eframe::NativeOptions {
        renderer,
        persist_window: true,
        wgpu_options: wgpu_config(),
        viewport: egui::ViewportBuilder::default()
            .with_title("Zoetrope")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 320.0])
            .with_icon(app_icon())
            .with_app_id("zoetrope"),
        ..Default::default()
    };

    eframe::run_native(
        "Zoetrope",
        options,
        Box::new(move |cc| {
            install_cjk_fonts(&cc.egui_ctx);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(zoetrope::app::ViewerApp::new(
                cc,
                path,
                renderer_label,
            )))
        }),
    )
}

/// 載入系統 CJK 字型，讓中文檔名與介面正常顯示（egui 內建字型不含漢字）
fn install_cjk_fonts(ctx: &egui::Context) {
    let candidates: &[(&str, u32)] = if cfg!(target_os = "windows") {
        &[
            ("C:/Windows/Fonts/msjh.ttc", 0),    // 微軟正黑體
            ("C:/Windows/Fonts/msyh.ttc", 0),    // 微軟雅黑
            ("C:/Windows/Fonts/mingliu.ttc", 0), // 細明體
            ("C:/Windows/Fonts/simhei.ttf", 0),
        ]
    } else if cfg!(target_os = "macos") {
        &[
            ("/System/Library/Fonts/PingFang.ttc", 0),
            ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
            ("/System/Library/Fonts/STHeiti Light.ttc", 0),
        ]
    } else {
        &[
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
            ("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc", 0),
            (
                "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
                0,
            ),
        ]
    };
    for (path, index) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            let mut data = egui::FontData::from_owned(bytes);
            data.index = *index;
            fonts.font_data.insert("cjk".to_owned(), data);
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("cjk".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}

/// 我們願意要求的最大貼圖邊長。egui-wgpu 預設把
/// `max_texture_dimension_2d` 寫死成 8192（不查硬體），
/// 導致超過 8192px 的圖一律被縮小、1:1 檢視變糊。
/// 這裡改成「硬體支援多少就要多少」，上限 16384——
/// 絕大多數桌面 GPU 都支援，可讓一般大圖完全不必降階。
const WANTED_MAX_TEXTURE: u32 = 16384;

fn wgpu_config() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::wgpu;
    eframe::egui_wgpu::WgpuConfiguration {
        device_descriptor: std::sync::Arc::new(|adapter: &wgpu::Adapter| {
            let base = if adapter.get_info().backend == wgpu::Backend::Gl {
                wgpu::Limits::downlevel_webgl2_defaults()
            } else {
                wgpu::Limits::default()
            };
            // 不能要求超過硬體上限，否則裝置建立會失敗；
            // 也不要低於 egui-wgpu 原本要求的 8192。
            let want = adapter
                .limits()
                .max_texture_dimension_2d
                .clamp(8192, WANTED_MAX_TEXTURE);
            wgpu::DeviceDescriptor {
                label: Some("zoetrope wgpu device"),
                required_features: wgpu::Features::default(),
                required_limits: wgpu::Limits {
                    max_texture_dimension_2d: want,
                    ..base
                },
                memory_hints: wgpu::MemoryHints::default(),
            }
        }),
        ..Default::default()
    }
}

/// 視窗圖示（與 assets/icon.ico 同一份程式化圖形）
fn app_icon() -> egui::IconData {
    const S: u32 = 64;
    egui::IconData {
        rgba: zoetrope::appicon::icon_rgba(S),
        width: S,
        height: S,
    }
}
