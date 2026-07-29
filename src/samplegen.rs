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
    landscape(w, h).save(&p).map_err(io_err)?;
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
        Rgba([(255.0 * y as f32 / h.max(1) as f32) as u8, 140, 220, a])
    })
}

/// 程式化產生的日落山景：天空漸層 + 太陽光暈 + 多層山稜 + 星點。
/// 用來當示範／截圖用圖，比隨機雜訊好看得多，且完全由程式碼產生（無版權疑慮）。
fn landscape(w: u32, h: u32) -> RgbImage {
    let (fw, fh) = (w as f32, h as f32);
    let horizon = fh * 0.62;
    let sun = (fw * 0.70, horizon - fh * 0.16);
    let sun_r = fh * 0.075;

    // 山稜線：幾個不同頻率的正弦疊加，每層有各自的相位與基準高度
    let ridge_y = |layer: usize, x: f32| -> f32 {
        let p = layer as f32 * 2.7;
        let base = horizon + fh * (0.02 + 0.075 * layer as f32);
        let amp = fh * (0.075 - 0.012 * layer as f32).max(0.02);
        let n = (x / fw * 6.0 + p).sin()
            + 0.5 * (x / fw * 13.0 + p * 1.7).sin()
            + 0.25 * (x / fw * 27.0 + p * 2.3).sin();
        base - amp * n
    };
    // 由遠而近，山色越來越深
    let ridge_col = [
        (74.0, 96.0, 122.0),
        (52.0, 72.0, 98.0),
        (34.0, 50.0, 72.0),
        (20.0, 30.0, 46.0),
    ];

    RgbImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);

        // 天空：由深靛藍漸層到地平線的暖橘
        let t = (fy / horizon).clamp(0.0, 1.0);
        let e = t * t; // 讓暖色集中在接近地平線處
        let mut c = (26.0 + 214.0 * e, 32.0 + 130.0 * e, 64.0 + 40.0 * e);

        // 星點：只出現在上半部較暗的天空
        if fy < horizon * 0.55 {
            let hash = ((x.wrapping_mul(73_856_093)) ^ (y.wrapping_mul(19_349_663))) % 4001;
            if hash == 0 {
                let b = 200.0 + (hash % 55) as f32;
                c = (b, b, b * 0.95);
            }
        }

        // 太陽光暈（加成混合）+ 本體
        let d = ((fx - sun.0).powi(2) + (fy - sun.1).powi(2)).sqrt();
        if d < sun_r * 9.0 {
            let glow = (1.0 - d / (sun_r * 9.0)).powf(2.6);
            c.0 = (c.0 + 235.0 * glow).min(255.0);
            c.1 = (c.1 + 150.0 * glow).min(255.0);
            c.2 = (c.2 + 55.0 * glow).min(255.0);
        }
        if d < sun_r {
            let edge = (1.0 - (d / sun_r).powf(8.0)).clamp(0.0, 1.0);
            c.0 = c.0 * (1.0 - edge) + 255.0 * edge;
            c.1 = c.1 * (1.0 - edge) + 238.0 * edge;
            c.2 = c.2 * (1.0 - edge) + 190.0 * edge;
        }

        // 山稜：由遠到近覆蓋
        for (i, col) in ridge_col.iter().enumerate() {
            if fy >= ridge_y(i, fx) {
                // 每層加一點垂直漸層，避免死板的色塊
                let depth = ((fy - ridge_y(i, fx)) / fh).clamp(0.0, 1.0);
                c = (
                    col.0 * (1.0 - depth * 0.5),
                    col.1 * (1.0 - depth * 0.5),
                    col.2 * (1.0 - depth * 0.5),
                );
            }
        }

        Rgb([c.0 as u8, c.1 as u8, c.2 as u8])
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
