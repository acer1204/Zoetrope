//! 驗證 JPEG DCT 縮放解碼的正確性與速度優勢。

use std::time::Instant;

use zoetrope::jpeg_fast;

fn tmp() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("zoetrope-jprog-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 產生一張有真實影像特徵（漸層＋幾何）的大 JPEG
fn big_jpeg(dir: &std::path::Path, w: u32, h: u32) -> std::path::PathBuf {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let ring = ((fx - 0.5).powi(2) + (fy - 0.5).powi(2)).sqrt() * 40.0;
        image::Rgb([
            (255.0 * fx) as u8,
            (128.0 + 127.0 * ring.sin()) as u8,
            (255.0 * fy) as u8,
        ])
    });
    let p = dir.join(format!("big_{w}x{h}.jpg"));
    image::DynamicImage::ImageRgb8(img)
        .save_with_format(&p, image::ImageFormat::Jpeg)
        .unwrap();
    p
}

#[test]
fn dct_scaling_is_substantially_faster() {
    let dir = tmp();
    let path = big_jpeg(&dir, 6000, 4000);

    // 全解析度
    let t = Instant::now();
    let (full, full_size) = jpeg_fast::decode_scaled(&path, 1).expect("全解析度解碼");
    let full_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(full_size, (6000, 4000));

    // 漸進式第一階段會挑的倍率（與 loader 的 JPEG_FAST_TARGET 一致）
    let scale = jpeg_fast::pick_scale(full_size, 1200);
    assert_eq!(scale, 4, "6000px 的圖應落在 1/4，實際 scale={scale}");

    let t = Instant::now();
    let (small, small_size) = jpeg_fast::decode_scaled(&path, scale).expect("縮放解碼");
    let fast_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(small_size, (6000 / scale, 4000 / scale));
    assert_eq!((small.width(), small.height()), small_size);

    eprintln!(
        "6000×4000 JPEG：全解析度 {full_ms:.0}ms → 1/{scale} 縮放 {fast_ms:.0}ms（{:.1}× 快）",
        full_ms / fast_ms.max(0.001)
    );
    // 各倍率的實際耗時（供調整 JPEG_FAST_TARGET 參考）
    for s in [2u32, 4, 8] {
        let t = Instant::now();
        let (img, sz) = jpeg_fast::decode_scaled(&path, s).expect("縮放解碼");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "  1/{s}: {}×{}  {ms:.0}ms（{:.1}× 快）",
            sz.0,
            sz.1,
            full_ms / ms.max(0.001)
        );
        let _ = img;
    }

    // 縮放解碼必須明顯更快，否則漸進式沒有意義
    assert!(
        fast_ms < full_ms * 0.7,
        "縮放解碼沒有明顯加速：{fast_ms:.0}ms vs {full_ms:.0}ms"
    );

    // 內容要一致：兩者取樣後的平均色應接近
    let mean = |img: &image::RgbaImage, ch: usize| -> f64 {
        let s: u64 = img.pixels().step_by(31).map(|p| p.0[ch] as u64).sum();
        s as f64 / img.pixels().step_by(31).count() as f64
    };
    for ch in 0..3 {
        let (a, b) = (mean(&full, ch), mean(&small, ch));
        assert!(
            (a - b).abs() < 12.0,
            "通道 {ch} 內容偏差過大：{a:.1} vs {b:.1}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 小圖不該走漸進式（多解一次反而慢）
#[test]
fn small_images_skip_progressive_path() {
    assert_eq!(jpeg_fast::pick_scale((1600, 1200), 1200), 1);
    assert_eq!(jpeg_fast::pick_scale((2000, 1500), 1200), 1);
    // 2400 以上開始有縮放空間
    assert_eq!(jpeg_fast::pick_scale((2400, 1800), 1200), 2);
    assert_eq!(jpeg_fast::pick_scale((6000, 4000), 1200), 4);
}

/// 主解碼路徑對大 JPEG 的最終結果必須是全解析度
#[test]
fn main_path_ends_at_full_resolution() {
    let dir = tmp();
    let path = big_jpeg(&dir, 4096, 3072);
    let (img, fmt) = zoetrope::loader::decode_static(&path).expect("主路徑解碼");
    assert_eq!(fmt, "JPEG");
    assert_eq!(
        (img.width(), img.height()),
        (4096, 3072),
        "最終結果應為全解析度"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
