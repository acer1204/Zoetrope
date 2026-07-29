//! 測試／示範用圖片產生器（也是 examples/gen_samples 與整合測試的共用邏輯）

use std::path::{Path, PathBuf};

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, Rgb, RgbImage, Rgba, RgbaImage};

fn io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// 在 `dir` 產生一組涵蓋主要路徑的樣本；`big` = 產生接近實用尺寸的大圖。
/// 檔名故意用 1/2/10/100 測自然排序。
pub fn write_all(dir: &Path, big: bool) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::new();

    // 1. 含透明的 PNG
    let p = dir.join("sample_1_alpha.png");
    let (w, h) = if big { (800, 600) } else { (96, 64) };
    gradient_alpha(w, h).save(&p).map_err(io_err)?;
    out.push(p);

    // 2. 動畫 GIF（測串流解碼與播放）
    let p = dir.join("sample_2_anim.gif");
    let (w, h, n) = if big { (640, 360, 150) } else { (64, 48, 8) };
    animated_gif(&p, w, h, n).map_err(io_err)?;
    out.push(p);

    // 3. 大張 JPEG（測 mip 鏈與縮圖品質）
    let p = dir.join("sample_10_photo.jpg");
    let (w, h) = if big { (4000, 2600) } else { (160, 100) };
    plasma(w, h).save(&p).map_err(io_err)?;
    out.push(p);

    // 4. 靜態 WebP
    let p = dir.join("sample_100_tiles.webp");
    let (w, h) = if big { (1024, 768) } else { (80, 60) };
    tiles(w, h).save(&p).map_err(io_err)?;
    out.push(p);

    Ok(out)
}

pub fn animated_gif(path: &Path, w: u32, h: u32, frames: usize) -> image::ImageResult<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = GifEncoder::new_with_speed(file, 30);
    enc.set_repeat(Repeat::Infinite)?;
    for i in 0..frames {
        let img = ball_frame(w, h, i, frames);
        enc.encode_frame(Frame::from_parts(
            img,
            0,
            0,
            Delay::from_numer_denom_ms(40, 1),
        ))?;
    }
    Ok(())
}

fn ball_frame(w: u32, h: u32, i: usize, n: usize) -> RgbaImage {
    let t = i as f32 / n.max(1) as f32 * std::f32::consts::TAU;
    let (cx, cy) = (
        w as f32 * (0.5 + 0.3 * t.cos()),
        h as f32 * (0.5 + 0.3 * t.sin()),
    );
    let r = h as f32 * 0.12;
    RgbaImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32, y as f32);
        let d2 = (fx - cx).powi(2) + (fy - cy).powi(2);
        if d2 < r * r {
            Rgba([255, 220, 90, 255])
        } else {
            Rgba([
                (40.0 + 120.0 * fx / w as f32) as u8,
                (30.0 + 60.0 * fy / h as f32) as u8,
                (90.0 + 120.0 * (1.0 - fx / w as f32)) as u8,
                255,
            ])
        }
    })
}

fn gradient_alpha(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let a = (255.0 * x as f32 / w.max(1) as f32) as u8;
        Rgba([
            (255.0 * y as f32 / h.max(1) as f32) as u8,
            140,
            220,
            a,
        ])
    })
}

fn plasma(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 * 0.011, y as f32 * 0.013);
        let v = (fx.sin() + fy.cos() + (fx * 0.7 + fy * 0.6).sin()) / 3.0;
        Rgb([
            (128.0 + 127.0 * v) as u8,
            (128.0 + 127.0 * (v + 0.7).sin()) as u8,
            (128.0 + 127.0 * (v * 2.1).cos()) as u8,
        ])
    })
}

fn tiles(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let odd = ((x / 32) + (y / 32)) % 2 == 0;
        if odd {
            Rgba([64, 160, 168, 255])
        } else {
            Rgba([236, 240, 244, 255])
        }
    })
}
