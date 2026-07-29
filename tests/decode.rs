//! 端到端解碼測試：產生樣本 → 掃描資料夾 → 解碼驗證

use std::path::PathBuf;

use zoetrope::{dirlist, loader, samplegen};

fn tmp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("zoetrope-test-{tag}-{}", std::process::id()))
}

#[test]
fn scan_decode_roundtrip() {
    let dir = tmp_dir("roundtrip");
    let _ = std::fs::remove_dir_all(&dir);
    let files = samplegen::write_all(&dir, false).expect("樣本產生");
    assert_eq!(files.len(), 4);

    // 資料夾掃描（含中繼資料）+ 自然排序（1 < 2 < 10 < 100）
    let mut entries = dirlist::scan_dir(&dir);
    assert!(
        entries.iter().all(|e| e.size > 0 && e.modified.is_some()),
        "掃描應帶出大小與修改時間"
    );
    dirlist::sort_entries(&mut entries, dirlist::SortKey::Name, true);
    let scanned: Vec<std::path::PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
    let names: Vec<String> = scanned
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "sample_1_alpha.png",
            "sample_2_anim.gif",
            "sample_10_photo.jpg",
            "sample_100_tiles.webp",
        ]
    );

    // 靜態解碼：PNG（含 alpha）
    let (png, fmt) = loader::decode_static(&scanned[0]).expect("png 解碼");
    assert_eq!((png.width(), png.height()), (96, 64));
    assert_eq!(fmt, "PNG");
    assert!(png.pixels().any(|p| p.0[3] != 255), "PNG 應含透明像素");

    // 靜態解碼：JPEG / WebP
    let (jpg, _) = loader::decode_static(&scanned[2]).expect("jpeg 解碼");
    assert_eq!((jpg.width(), jpg.height()), (160, 100));
    let (webp, fmt) = loader::decode_static(&scanned[3]).expect("webp 解碼");
    assert_eq!((webp.width(), webp.height()), (80, 60));
    assert_eq!(fmt, "WEBP");

    // 動畫 GIF：影格串流疊代器
    let anim = loader::open_animation(&scanned[1], "gif").expect("gif 開啟");
    let (frames, dims, fmt) = anim.expect("gif 應判定為動畫");
    assert_eq!(dims, (64, 48));
    assert_eq!(fmt, "GIF");
    let frames: Vec<_> = frames.collect::<Result<Vec<_>, _>>().expect("gif 影格解碼");
    assert_eq!(frames.len(), 8);
    for f in &frames {
        let buf = f.buffer();
        assert_eq!((buf.width(), buf.height()), (64, 48));
    }

    // 靜態 WebP 不應誤判為動畫
    assert!(loader::open_animation(&scanned[3], "webp")
        .expect("webp 開啟")
        .is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn misnamed_jpeg_as_png_decodes() {
    // Instagram 等來源常見：JPEG 內容存成 .png 副檔名，必須以內容為準
    let dir = tmp_dir("misnamed");
    let _ = std::fs::remove_dir_all(&dir);
    let files = samplegen::write_all(&dir, false).expect("樣本產生");
    let jpg = files
        .iter()
        .find(|p| p.extension().unwrap() == "jpg")
        .unwrap();
    let fake_png = dir.join("actually_jpeg.png");
    std::fs::copy(jpg, &fake_png).unwrap();

    // 動畫探測不應誤判也不應報錯
    assert!(loader::open_animation(&fake_png, "png")
        .expect("探測不應失敗")
        .is_none());
    // 靜態解碼以內容偵測成功，格式回報真實格式
    let (img, fmt) = loader::decode_static(&fake_png).expect("misnamed 檔應可解碼");
    assert_eq!((img.width(), img.height()), (160, 100));
    assert_eq!(fmt, "JPEG");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clamp_oversized_texture() {
    // 超過貼圖上限的圖要被縮到上限以內
    let big = image::RgbaImage::new(9000, 120);
    let ci = loader::to_color_image_clamped(big);
    assert!(ci.size[0] <= zoetrope::types::MAX_TEX_DIM as usize);
    assert_eq!(ci.size[0], 4500);
    assert_eq!(ci.size[1], 60);
}
