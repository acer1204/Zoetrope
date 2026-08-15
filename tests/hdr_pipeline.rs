//! HDR / EXR 端到端測試：產生真實 EXR → 解碼 → 色調映射 → 驗證亮度正確。
//!
//! 重點是驗證「修好的 bug」：舊做法把線性值直接截斷成 0–255 且不做 gamma，
//! 導致整張偏暗、亮部糊成一片白。

use std::time::Instant;

use zoetrope::hdr::{self, ToneOp};

/// 每個測試自己的暫存目錄。測試平行執行且結尾各自 remove_dir_all，
/// 共用目錄會被先跑完的測試連根刪掉，所以 tag 必須每個測試唯一。
fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("zoetrope-hdr-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 產生一張有明確亮度階梯的 EXR：從 0.01 到 64.0，橫跨 12 個 stop。
///
/// 檔名帶上尺寸——測試是平行跑的，而同一個執行檔裡 PID 相同，
/// 共用檔名會讓一個測試讀到另一個測試寫到一半的檔案。
fn make_exr(dir: &std::path::Path, w: u32, h: u32) -> std::path::PathBuf {
    let img = image::Rgb32FImage::from_fn(w, h, |x, _| {
        // 每一欄一個亮度，指數分布
        let t = x as f32 / (w.max(2) - 1) as f32;
        let v = 0.01f32 * (64.0f32 / 0.01).powf(t);
        image::Rgb([v, v, v])
    });
    let p = dir.join(format!("steps-{w}x{h}.exr"));
    image::DynamicImage::ImageRgb32F(img)
        .save_with_format(&p, image::ImageFormat::OpenExr)
        .unwrap();
    p
}

#[test]
fn exr_decodes_to_float_and_tonemaps_correctly() {
    let dir = tmp("tonemap");
    let path = make_exr(&dir, 64, 8);

    // 解碼保留浮點
    let img = image::open(&path).expect("EXR 解碼");
    let h = hdr::from_dynamic(&img).expect("應取得浮點資料");
    assert_eq!(h.size, [64, 8]);

    // 最亮的那一欄確實遠超過 1.0（這正是 8-bit 表達不了的部分）
    let last = hdr::f16_to_f32(h.px[(63 * 4) as usize]);
    assert!(last > 32.0, "最亮欄應 >32，實際 {last}");

    let lut = hdr::ToneLut::build(0.0, ToneOp::Aces);
    let out = hdr::tonemap(&h, &lut);
    assert_eq!(out.size, [64, 8]);

    // 1. 亮度必須單調遞增（色調映射不能把順序弄亂）
    let row: Vec<u8> = (0..64).map(|x| out.pixels[x].r()).collect();
    for i in 1..row.len() {
        assert!(
            row[i] >= row[i - 1],
            "第 {i} 欄亮度倒退：{:?}",
            &row[i - 5..=i]
        );
    }

    // 2. 高光壓縮：ACES 飽和的欄位必須明顯少於截斷法。
    //    （ACES 對超過約 6 倍白的值仍會到 255，那是正確的顯示轉換行為；
    //     真正的差別在於 1.0～6.0 這段被保留下來，截斷法則整段死白。）
    let clip_lut = hdr::ToneLut::build(0.0, ToneOp::Clip);
    let clip_out = hdr::tonemap(&h, &clip_lut);
    let sat_aces = row.iter().filter(|&&v| v == 255).count();
    let sat_clip = (0..64).filter(|&x| clip_out.pixels[x].r() == 255).count();
    assert!(
        sat_aces + 8 <= sat_clip,
        "ACES 應保留更多高光細節：ACES 飽和 {sat_aces} 欄 vs 截斷 {sat_clip} 欄"
    );

    // 落在 1.0～6.0 之間的值，ACES 要能區分出不同亮度，截斷法則全部是 255
    let probe = [1.5f32, 2.5, 4.0];
    let aces_vals: Vec<u8> = probe
        .iter()
        .map(|v| lut.color(hdr::f32_to_f16(*v)))
        .collect();
    let clip_vals: Vec<u8> = probe
        .iter()
        .map(|v| clip_lut.color(hdr::f32_to_f16(*v)))
        .collect();
    assert_eq!(clip_vals, vec![255, 255, 255], "截斷法在 >1.0 應全部死白");
    assert!(
        aces_vals[0] < aces_vals[1] && aces_vals[1] < aces_vals[2],
        "ACES 應能區分 1.5/2.5/4.0 的亮度差異：{aces_vals:?}"
    );

    // 3. 中灰 0.18 附近應落在合理的顯示亮度（sRGB 編碼後約 118–130）
    //    舊做法沒做 gamma，0.18 會變成 46——明顯偏暗，這正是原本的 bug
    let mid_bits = hdr::f32_to_f16(0.18);
    let mid = lut.color(mid_bits);
    assert!(
        (100..=150).contains(&mid),
        "中灰 0.18 應約 118–130（含 gamma），實際 {mid}；\
         若接近 46 代表 gamma 沒做"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn main_decode_path_handles_exr() {
    let dir = tmp("decode");
    let path = make_exr(&dir, 32, 32);
    // 主解碼入口目前對 HDR 仍回 8-bit（decode_static 是舊路徑），
    // 真正的 HDR 流程在 decode_streaming；這裡只確認不會出錯
    let r = zoetrope::loader::decode_static(&path);
    assert!(r.is_ok(), "EXR 應可解碼：{:?}", r.err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 曝光調整必須夠快才能做成即時滑桿
#[test]
fn exposure_adjustment_is_fast_enough_for_a_slider() {
    let dir = tmp("slider");
    // 4K 等級
    let path = make_exr(&dir, 2048, 1080);
    let img = image::open(&path).expect("EXR 解碼");
    let h = hdr::from_dynamic(&img).expect("浮點資料");

    let t = Instant::now();
    let lut = hdr::ToneLut::build(1.5, ToneOp::Aces);
    let lut_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let out = hdr::tonemap(&h, &lut);
    let map_ms = t.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(out.size, [2048, 1080]);
    eprintln!(
        "{}×{}：LUT 重建 {lut_ms:.1}ms，色調映射 {map_ms:.1}ms，合計 {:.1}ms",
        h.size[0],
        h.size[1],
        lut_ms + map_ms
    );

    // 拉滑桿要跟得上——放寬到 150ms 以容忍 CI 機器較慢
    assert!(
        lut_ms + map_ms < 150.0,
        "曝光調整太慢（{:.0}ms），滑桿會頓",
        lut_ms + map_ms
    );

    let _ = std::fs::remove_dir_all(&dir);
}
