//! JPEG XR 辨識與解碼測試。
//!
//! 格式辨識是純邏輯，隨時可跑。實際解碼需要真實的 .jxr 檔——
//! 把樣本放進 `tests/assets/` 就會自動納入驗證（沒有時跳過而非失敗）。
//!
//! 產生樣本檔的方式（Windows 內建，不需額外工具）：
//! ```powershell
//! Add-Type -AssemblyName PresentationCore
//! # 見 README「測試」章節的完整腳本
//! ```

use std::path::Path;

use zoetrope::jxr::{self, JxrImage};

#[test]
fn recognises_jxr_by_extension_and_header() {
    assert!(jxr::is_jxr_path(Path::new("shot.jxr")));
    assert!(jxr::is_jxr_path(Path::new("SHOT.JXR")));
    assert!(jxr::is_jxr_path(Path::new("legacy.wdp")));
    assert!(!jxr::is_jxr_path(Path::new("photo.png")));

    // 真實 JXR 檔頭（由 Windows 的 WmpBitmapEncoder 產生並驗證過）
    assert!(jxr::is_jxr_header(&[0x49, 0x49, 0xBC, 0x01]));
    // 一般 TIFF 是 II 2A 00，不該被攔截
    assert!(!jxr::is_jxr_header(&[0x49, 0x49, 0x2A, 0x00]));
    assert!(!jxr::is_jxr_header(b"II"));
    assert!(!jxr::is_jxr_header(&[]));
}

#[test]
fn dirlist_includes_jxr() {
    assert!(zoetrope::dirlist::is_image_path(Path::new("a.jxr")));
    assert!(zoetrope::dirlist::is_image_path(Path::new("a.wdp")));
}

/// 有真實樣本時，驗證解碼與色調映射整條路徑
#[test]
fn decodes_real_jxr_samples_if_present() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/assets");
    let Ok(rd) = std::fs::read_dir(&assets) else {
        eprintln!("跳過：tests/assets 不存在（放入 .jxr 樣本即會納入測試）");
        return;
    };
    let mut checked = 0;
    for e in rd.flatten() {
        let p = e.path();
        if !jxr::is_jxr_path(&p) {
            continue;
        }
        let img = jxr::decode(&p).unwrap_or_else(|err| panic!("{} 解碼失敗：{err}", p.display()));
        match img {
            JxrImage::Hdr(h) => {
                assert!(h.size[0] > 0 && h.size[1] > 0);
                assert_eq!(h.px.len(), h.size[0] * h.size[1] * 4);
                // 接上色調映射應產生合法影像
                let lut = zoetrope::hdr::ToneLut::build(0.0, zoetrope::hdr::ToneOp::Aces);
                let out = zoetrope::hdr::tonemap(&h, &lut);
                assert_eq!(out.size, h.size);
                let max =
                    h.px.chunks_exact(4)
                        .map(|c| zoetrope::hdr::f16_to_f32(c[0]))
                        .fold(0f32, f32::max);
                eprintln!(
                    "OK {} → HDR {}×{}，最亮 {max:.2}",
                    p.file_name().unwrap().to_string_lossy(),
                    h.size[0],
                    h.size[1]
                );
            }
            JxrImage::Sdr(rgba) => {
                assert!(rgba.width() > 0 && rgba.height() > 0);
                eprintln!(
                    "OK {} → SDR {}×{}",
                    p.file_name().unwrap().to_string_lossy(),
                    rgba.width(),
                    rgba.height()
                );
            }
        }
        checked += 1;
    }
    eprintln!("已驗證 {checked} 個 JXR 樣本");
}

/// 非 JXR 檔案不該被 JXR 解碼器接手
#[test]
fn rejects_non_jxr_input() {
    let dir = std::env::temp_dir().join(format!("zoetrope-jxrneg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("fake.jxr");
    std::fs::write(&p, b"not an image at all").unwrap();
    assert!(jxr::decode(&p).is_err(), "非 JXR 內容應回報錯誤而非崩潰");
    let _ = std::fs::remove_dir_all(&dir);
}
