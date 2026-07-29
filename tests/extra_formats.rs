//! 新格式的端到端解碼測試。
//!
//! AVIF 可用純 Rust 編碼器（ravif）現場產生真實檔案，因此完整驗證
//! 「編碼 → 偵測 → 解碼 → 像素正確」整條路徑。
//! JPEG XL / HEIC / RAW 沒有純 Rust 編碼器可產生樣本，若把真實樣本檔
//! 放進 tests/assets/ 便會自動納入測試（否則該項跳過）。

use std::path::{Path, PathBuf};

use zoetrope::extra_formats::{self, ExtraFormat};

fn tmp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("zoetrope-xfmt-{tag}-{}", std::process::id()))
}

/// 產生一張有明確色塊的測試圖，方便驗證解碼後的像素值
fn test_pixels(w: usize, h: usize) -> Vec<rgb::RGBA8> {
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            if x < w / 2 && y < h / 2 {
                rgb::RGBA8::new(220, 30, 40, 255) // 左上：紅
            } else if y < h / 2 {
                rgb::RGBA8::new(30, 200, 60, 255) // 右上：綠
            } else if x < w / 2 {
                rgb::RGBA8::new(40, 60, 220, 255) // 左下：藍
            } else {
                rgb::RGBA8::new(240, 240, 240, 255) // 右下：白
            }
        })
        .collect()
}

#[test]
fn avif_roundtrip_encode_detect_decode() {
    let (w, h) = (64usize, 48usize);
    let pixels = test_pixels(w, h);
    let encoded = ravif::Encoder::new()
        .with_quality(90.0)
        .with_speed(10)
        .encode_rgba(ravif::Img::new(&pixels[..], w, h))
        .expect("AVIF 編碼");

    let dir = tmp_dir("avif");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.avif");
    std::fs::write(&path, &encoded.avif_file).unwrap();

    // 1. 格式偵測（走檔頭 ftyp/avif）
    assert_eq!(
        extra_formats::sniff(&path),
        Some(ExtraFormat::Avif),
        "應以檔頭辨識為 AVIF"
    );

    // 2. 解碼
    let img = extra_formats::decode(&path, ExtraFormat::Avif).expect("AVIF 解碼");
    assert_eq!((img.width(), img.height()), (w as u32, h as u32));

    // 3. 像素正確性：四個象限的顏色應大致還原（有損壓縮，容忍誤差）
    let at = |x: u32, y: u32| img.get_pixel(x, y).0;
    let close = |got: [u8; 4], want: [u8; 3], label: &str| {
        for c in 0..3 {
            let diff = (got[c] as i32 - want[c] as i32).abs();
            assert!(
                diff <= 40,
                "{label} 通道{c} 差異過大：得到 {got:?}，預期約 {want:?}"
            );
        }
        assert_eq!(got[3], 255, "{label} alpha 應為不透明");
    };
    close(at(10, 10), [220, 30, 40], "左上紅");
    close(at(54, 10), [30, 200, 60], "右上綠");
    close(at(10, 38), [40, 60, 220], "左下藍");
    close(at(54, 38), [240, 240, 240], "右下白");

    // 4. 經由主解碼路徑（app 實際呼叫的入口）也要成功
    let (img2, fmt) = zoetrope::loader::decode_static(&path).expect("主路徑解碼 AVIF");
    assert_eq!(fmt, "AVIF");
    assert_eq!((img2.width(), img2.height()), (w as u32, h as u32));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 副檔名標錯的 AVIF（存成 .png）仍應以內容辨識並解碼
#[test]
fn avif_with_wrong_extension() {
    let (w, h) = (32usize, 32usize);
    let pixels = test_pixels(w, h);
    let encoded = ravif::Encoder::new()
        .with_quality(80.0)
        .with_speed(10)
        .encode_rgba(ravif::Img::new(&pixels[..], w, h))
        .expect("AVIF 編碼");

    let dir = tmp_dir("avif-misnamed");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("actually_avif.png");
    std::fs::write(&path, &encoded.avif_file).unwrap();

    assert_eq!(extra_formats::sniff(&path), Some(ExtraFormat::Avif));
    let (img, fmt) = zoetrope::loader::decode_static(&path).expect("解碼");
    assert_eq!(fmt, "AVIF");
    assert_eq!((img.width(), img.height()), (w as u32, h as u32));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 若 tests/assets/ 內放了真實樣本檔，逐一驗證可正確解碼。
/// 沒有樣本時此測試會列出跳過項目而不失敗。
#[test]
fn real_world_samples_if_present() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/assets");
    if !assets.is_dir() {
        eprintln!("跳過：tests/assets 不存在（放入 .jxl/.heic/.cr2 等樣本即會納入測試）");
        return;
    }
    let mut checked = 0;
    for entry in std::fs::read_dir(&assets).expect("讀取 assets") {
        let path = entry.expect("entry").path();
        if !path.is_file() {
            continue;
        }
        let Some(fmt) = extra_formats::sniff(&path) else {
            continue; // 一般格式交給既有測試
        };
        let img = extra_formats::decode(&path, fmt)
            .unwrap_or_else(|e| panic!("{} 解碼失敗（{}）：{e}", path.display(), fmt.name()));
        assert!(
            img.width() > 0 && img.height() > 0,
            "{} 解出的尺寸無效",
            path.display()
        );
        // 全透明或全黑通常代表解碼有問題
        let opaque = img.pixels().any(|p| p.0[3] > 0);
        assert!(opaque, "{} 解出的影像完全透明", path.display());
        eprintln!(
            "OK {} → {} {}×{}",
            path.file_name().unwrap().to_string_lossy(),
            fmt.name(),
            img.width(),
            img.height()
        );
        checked += 1;
    }
    eprintln!("已驗證 {checked} 個真實樣本檔");
}
