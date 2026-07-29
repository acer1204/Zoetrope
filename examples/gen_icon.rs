//! 產生多尺寸 assets/icon.ico（build.rs 會把它嵌進 Windows 執行檔）：
//! cargo run --release --example gen_icon

use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::ExtendedColorType;

fn main() {
    std::fs::create_dir_all("assets").expect("create assets dir");
    let sizes = [256u32, 64, 48, 32, 16];
    let frames: Vec<IcoFrame> = sizes
        .iter()
        .map(|&s| {
            let rgba = zoetrope::appicon::icon_rgba(s);
            IcoFrame::as_png(&rgba, s, s, ExtendedColorType::Rgba8).expect("encode ico frame")
        })
        .collect();
    let file = std::fs::File::create("assets/icon.ico").expect("create icon.ico");
    IcoEncoder::new(file)
        .encode_images(&frames)
        .expect("write icon.ico");
    println!("assets/icon.ico written ({} sizes)", sizes.len());
}
