//! 驗證「顯示參考」HDR（scRGB，Windows/NVIDIA 的 HDR 截圖）的轉換
//! 與 Windows 相簿一致。
//!
//! 背景：scRGB 的 1.0 **就是** SDR 白，超過的部分是 HDR 螢幕才顯示得出的高光。
//! Windows 的作法是直接截斷到 0–1 再做 sRGB 編碼；若誤套 ACES（那是給
//! 場景參考的 EXR 用的），中間調會整體被拉亮，畫面發灰。
//!
//! 這裡用實測數據把正確行為固定下來：一張真實的 NVIDIA HDR 截圖，
//! 來源 R 通道中位數 0.3052，Windows 輸出中位數 150。

use zoetrope::hdr::{self, HdrImage, HdrKind, ToneOp};

fn single(v: f32, kind: HdrKind) -> HdrImage {
    HdrImage {
        size: [1, 1],
        px: vec![
            hdr::f32_to_f16(v),
            hdr::f32_to_f16(v),
            hdr::f32_to_f16(v),
            hdr::f32_to_f16(1.0),
        ],
        kind,
    }
}

#[test]
fn display_referred_defaults_to_clip_not_aces() {
    assert_eq!(HdrKind::DisplayReferred.default_tone_op(), ToneOp::Clip);
    assert_eq!(HdrKind::SceneReferred.default_tone_op(), ToneOp::Aces);
}

/// 核心迴歸測試：真實截圖的中位數必須對得上 Windows 的輸出
#[test]
fn matches_windows_rendering_on_real_screenshot_values() {
    // 取自 3840×2160 的 NVIDIA HDR 截圖實測分佈
    // （來源浮點值 → Windows 相簿的 8-bit 輸出）
    let cases = [
        (0.0f32, 0u8),  // 純黑
        (0.010_1, 26),  // p01
        (0.305_2, 150), // p50 ← 最關鍵的一點
        (1.0, 255),     // SDR 白
        (2.24, 255),    // p90，超過 1.0 一律截斷
        (2.5, 255),     // 最大值
    ];

    let lut = hdr::ToneLut::build(0.0, HdrKind::DisplayReferred.default_tone_op());
    for (src, expect) in cases {
        let got = hdr::tonemap(&single(src, HdrKind::DisplayReferred), &lut).pixels[0].r();
        assert!(
            got.abs_diff(expect) <= 2,
            "來源 {src} 應輸出約 {expect}（Windows 相簿的結果），實際 {got}"
        );
    }
}

/// 對照：若誤用 ACES，中間調會明顯偏亮——這正是修正前的症狀
#[test]
fn aces_on_display_referred_content_is_visibly_too_bright() {
    let src = 0.305_2;
    let correct = hdr::tonemap(
        &single(src, HdrKind::DisplayReferred),
        &hdr::ToneLut::build(0.0, ToneOp::Clip),
    )
    .pixels[0]
        .r();
    let wrong = hdr::tonemap(
        &single(src, HdrKind::DisplayReferred),
        &hdr::ToneLut::build(0.0, ToneOp::Aces),
    )
    .pixels[0]
        .r();
    assert!(
        wrong > correct + 20,
        "ACES 套在顯示參考內容上應明顯偏亮（這是修正前的 bug）：\
         截斷 {correct} vs ACES {wrong}"
    );
}

/// 場景參考的 EXR 仍應走 ACES，高光才不會整片死白
#[test]
fn scene_referred_still_uses_aces_for_highlight_rolloff() {
    let lut = hdr::ToneLut::build(0.0, HdrKind::SceneReferred.default_tone_op());
    let a = hdr::tonemap(&single(1.5, HdrKind::SceneReferred), &lut).pixels[0].r();
    let b = hdr::tonemap(&single(3.0, HdrKind::SceneReferred), &lut).pixels[0].r();
    assert!(a < b, "ACES 應能區分 1.5 與 3.0：{a} vs {b}");
    assert!(b < 255, "3.0 不該直接死白：{b}");
}

/// 縮小影像時 kind 必須保留，否則 mip 層會用錯曲線
#[test]
fn halving_preserves_kind() {
    let img = HdrImage {
        size: [2, 2],
        px: vec![hdr::f32_to_f16(0.5); 16],
        kind: HdrKind::DisplayReferred,
    };
    assert_eq!(img.halved().kind, HdrKind::DisplayReferred);
}

/// 曝光補償仍可用來救回被截斷的高光
#[test]
fn exposure_recovers_clipped_highlights() {
    let src = 2.0f32; // 超過 1.0，預設會被截斷成白
    let at0 = hdr::tonemap(
        &single(src, HdrKind::DisplayReferred),
        &hdr::ToneLut::build(0.0, ToneOp::Clip),
    )
    .pixels[0]
        .r();
    assert_eq!(at0, 255, "0 EV 時應為白");

    // 降 2 EV（×0.25）後 2.0 變成 0.5，細節就回來了
    let down = hdr::tonemap(
        &single(src, HdrKind::DisplayReferred),
        &hdr::ToneLut::build(-2.0, ToneOp::Clip),
    )
    .pixels[0]
        .r();
    assert!(
        (170..=200).contains(&down),
        "降 2 EV 後應顯示出高光細節（約 188），實際 {down}"
    );
}
