//! JPEG 的 DCT 縮放解碼。
//!
//! JPEG 以 8×8 的 DCT 區塊儲存，解碼時可以只取用低頻係數做 1/2、1/4、1/8 的
//! 反轉換——**縮小的版本並不是「解完再縮」，而是根本沒解那麼多資料**，
//! 因此 1/8 解碼約比全解析度快一個數量級。
//!
//! 用途：開大圖時先解出「剛好覆蓋視窗」的版本讓畫面立刻出現，
//! 背景再解全解析度無縫替換。
//!
//! `image` crate 的 JPEG 走 zune-jpeg，並未提供縮放解碼，因此這裡直接用
//! `jpeg-decoder`。EXIF 方向仍沿用 `image` 的處理。

use std::fs::File;
use std::io::BufReader;

use image::RgbaImage;
use jpeg_decoder::{Decoder, PixelFormat};
use std::path::Path;

/// DCT 縮放只有 1/1、1/2、1/4、1/8 四段（jpeg-decoder 內部會挑最接近的）
const SCALE_STEPS: [u32; 4] = [1, 2, 4, 8];

/// 對 `(w, h)` 的原圖，挑一個能覆蓋 `target` 最長邊的最大縮小倍率。
/// 回傳 1 代表不需縮小。
pub fn pick_scale(orig: (u32, u32), target: u32) -> u32 {
    if target == 0 {
        return 1;
    }
    let long = orig.0.max(orig.1);
    let mut best = 1;
    for s in SCALE_STEPS {
        // 縮小後仍需 ≥ target 才不會顯得糊
        if long / s >= target {
            best = s;
        }
    }
    best
}

/// 只讀 JPEG 標頭取得尺寸，不解碼像素
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    let f = BufReader::new(File::open(path).ok()?);
    let mut dec = Decoder::new(f);
    dec.read_info().ok()?;
    let info = dec.info()?;
    Some((info.width as u32, info.height as u32))
}

/// 以 `scale` 倍縮小解碼（1 = 原尺寸）。回傳影像與其實際尺寸。
pub fn decode_scaled(path: &Path, scale: u32) -> Result<(RgbaImage, (u32, u32)), String> {
    let f = BufReader::new(File::open(path).map_err(|e| format!("無法開啟檔案：{e}"))?);
    let mut dec = Decoder::new(f);
    dec.read_info().map_err(|e| e.to_string())?;
    let info = dec.info().ok_or("JPEG 標頭不完整")?;

    if scale > 1 {
        // scale() 會挑最接近的 DCT 縮放段，回傳實際採用的尺寸
        let w = (info.width as u32 / scale).max(1) as u16;
        let h = (info.height as u32 / scale).max(1) as u16;
        dec.scale(w, h).map_err(|e| e.to_string())?;
    }

    let pixels = dec.decode().map_err(|e| e.to_string())?;
    let info = dec.info().ok_or("JPEG 解碼後標頭遺失")?;
    let (w, h) = (info.width as u32, info.height as u32);
    if w == 0 || h == 0 {
        return Err("JPEG 尺寸無效".into());
    }

    let expected = w as usize * h as usize;
    let img = match info.pixel_format {
        PixelFormat::RGB24 => {
            if pixels.len() < expected * 3 {
                return Err("JPEG 像素資料不足".into());
            }
            RgbaImage::from_fn(w, h, |x, y| {
                let s = (y as usize * w as usize + x as usize) * 3;
                image::Rgba([pixels[s], pixels[s + 1], pixels[s + 2], 255])
            })
        }
        PixelFormat::L8 => {
            if pixels.len() < expected {
                return Err("JPEG 像素資料不足".into());
            }
            RgbaImage::from_fn(w, h, |x, y| {
                let g = pixels[y as usize * w as usize + x as usize];
                image::Rgba([g, g, g, 255])
            })
        }
        PixelFormat::L16 => {
            if pixels.len() < expected * 2 {
                return Err("JPEG 像素資料不足".into());
            }
            RgbaImage::from_fn(w, h, |x, y| {
                let s = (y as usize * w as usize + x as usize) * 2;
                // 取高位元組降到 8-bit
                let g = pixels[s + 1];
                image::Rgba([g, g, g, 255])
            })
        }
        PixelFormat::CMYK32 => {
            if pixels.len() < expected * 4 {
                return Err("JPEG 像素資料不足".into());
            }
            // Adobe CMYK JPEG 多為反相儲存
            RgbaImage::from_fn(w, h, |x, y| {
                let s = (y as usize * w as usize + x as usize) * 4;
                let (c, m, ye, k) = (
                    pixels[s] as u32,
                    pixels[s + 1] as u32,
                    pixels[s + 2] as u32,
                    pixels[s + 3] as u32,
                );
                image::Rgba([
                    (c * k / 255) as u8,
                    (m * k / 255) as u8,
                    (ye * k / 255) as u8,
                    255,
                ])
            })
        }
    };
    Ok((img, (w, h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_jpeg(dir: &Path, name: &str, w: u32, h: u32) -> std::path::PathBuf {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            // 有結構的圖案，方便看出縮放是否正確
            let v = if (x / 32 + y / 32) % 2 == 0 { 230 } else { 40 };
            image::Rgb([v, (x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8])
        });
        let p = dir.join(name);
        image::DynamicImage::ImageRgb8(img)
            .save_with_format(&p, image::ImageFormat::Jpeg)
            .unwrap();
        p
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("zoetrope-jpg-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn pick_scale_covers_target() {
        // 4000px 長邊、想要至少 1000px → 1/4 剛好 1000
        assert_eq!(pick_scale((4000, 3000), 1000), 4);
        // 想要 1200 → 1/2 得 2000（1/4 只有 1000 不夠）
        assert_eq!(pick_scale((4000, 3000), 1200), 2);
        // 目標比原圖還大 → 不縮
        assert_eq!(pick_scale((800, 600), 1600), 1);
        // 極小目標 → 最多 1/8
        assert_eq!(pick_scale((4000, 3000), 10), 8);
        assert_eq!(pick_scale((4000, 3000), 0), 1);
    }

    #[test]
    fn reads_dimensions_without_decoding() {
        let d = tmp("dims");
        let p = write_jpeg(&d, "a.jpg", 640, 480);
        assert_eq!(dimensions(&p), Some((640, 480)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scaled_decode_produces_expected_sizes() {
        let d = tmp("scaled");
        let p = write_jpeg(&d, "big.jpg", 1600, 1200);

        let (full, size) = decode_scaled(&p, 1).unwrap();
        assert_eq!(size, (1600, 1200));
        assert_eq!((full.width(), full.height()), (1600, 1200));

        for s in [2u32, 4, 8] {
            let (img, size) = decode_scaled(&p, s).unwrap();
            assert_eq!(size, (1600 / s, 1200 / s), "1/{s} 尺寸不符");
            assert_eq!((img.width(), img.height()), size);
            assert_eq!(img.get_pixel(0, 0).0[3], 255, "alpha 應為不透明");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scaled_decode_keeps_content_recognisable() {
        // 縮小版與全解析度縮到同尺寸後，整體亮度應該接近
        let d = tmp("content");
        let p = write_jpeg(&d, "c.jpg", 800, 800);
        let (full, _) = decode_scaled(&p, 1).unwrap();
        let (small, _) = decode_scaled(&p, 4).unwrap();

        let mean = |img: &RgbaImage| -> f64 {
            let s: u64 = img.pixels().map(|p| p.0[0] as u64).sum();
            s as f64 / img.pixels().len() as f64
        };
        let (a, b) = (mean(&full), mean(&small));
        assert!(
            (a - b).abs() < 20.0,
            "縮放解碼的平均亮度偏差過大：{a:.1} vs {b:.1}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn grayscale_jpeg_decodes() {
        let d = tmp("gray");
        let img = image::GrayImage::from_fn(320, 240, |x, _| image::Luma([(x % 256) as u8]));
        let p = d.join("g.jpg");
        image::DynamicImage::ImageLuma8(img)
            .save_with_format(&p, image::ImageFormat::Jpeg)
            .unwrap();
        let (out, size) = decode_scaled(&p, 1).unwrap();
        assert_eq!(size, (320, 240));
        let px = out.get_pixel(100, 100).0;
        assert_eq!(px[0], px[1], "灰階應三通道相同");
        assert_eq!(px[1], px[2]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
