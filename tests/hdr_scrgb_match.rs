//! 驗證「顯示參考」HDR（scRGB，Windows/NVIDIA 的 HDR 截圖）的轉換
//! 與 Windows 相簿一致。
//!
//! 背景：scRGB 的 1.0 只有 80 nits，而 HDR 內容的「紙白」實際上落在
//! 200 nits 附近。轉成 SDR 時要把**紙白**對應到螢幕白，否則整張會偏亮約
//! 1.5 級。曲線方面則要用截斷而非 ACES——ACES 是給場景參考的 EXR 用的。
//!
//! 期望值是實測 Windows 相簿得來的：產生一張已知 scRGB 數值的階梯圖，
//! 用相簿開啟後從畫面讀回每一階的顏色。最關鍵的一點是相簿把 scRGB 1.0
//! 畫成 161（而非 255），由此反推出紙白約 226 nits。

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
fn display_referred_defaults_to_soft_not_aces() {
    assert_eq!(HdrKind::DisplayReferred.default_tone_op(), ToneOp::Soft);
    assert_eq!(HdrKind::SceneReferred.default_tone_op(), ToneOp::Aces);
}

/// 核心迴歸測試：輸出必須對得上 Windows 相簿**實際畫在螢幕上**的值。
///
/// 這組期望值是實測來的：產生一張已知 scRGB 數值的階梯圖，用相簿開啟，
/// 再從畫面上讀回每一階的顏色（見 scratchpad 的 stepwedge/readpatches）。
///
/// 關鍵發現：相簿並沒有把 scRGB 1.0 當成白（它畫成 161），因為 HDR 內容的
/// 「紙白」遠高於 scRGB 定義的 80 nits。把紙白對應到 SDR 白之後才會一致。
#[test]
fn matches_windows_photos_measured_output() {
    // (scRGB 輸入, Windows 相簿實際畫在螢幕上的值)
    // 這 10 個點涵蓋暗部、中間調到極亮高光，是整條曲線的形狀依據
    let cases = [
        (0.20f32, 77u8),
        (0.30, 92),
        (0.40, 106),
        (0.60, 128),
        (0.80, 146),
        (1.00, 161), // ← 1.0 不是白，這是最關鍵的一點
        (1.50, 193),
        (2.00, 213), // 以下三點決定高光肩部的形狀
        (4.00, 236), // 硬截斷在這裡會是 255，差了 19 級
        (8.00, 247),
    ];

    let kind = HdrKind::DisplayReferred;
    let lut = hdr::ToneLut::build(kind.default_exposure_ev(), kind.default_tone_op());
    let mut worst = 0u8;
    for (src, expect) in cases {
        let got = hdr::tonemap(&single(src, kind), &lut).pixels[0].r();
        worst = worst.max(got.abs_diff(expect));
        assert!(
            got.abs_diff(expect) <= 4,
            "來源 {src} 應輸出約 {expect}（相簿實測值），實際 {got}"
        );
    }
    eprintln!("與相簿的最大偏差：{worst}/255");
}

/// 高光滾降是「柔和」曲線存在的理由——硬截斷在皮膚等明亮區域會明顯偏亮
#[test]
fn soft_curve_rolls_off_highlights_unlike_clip() {
    let kind = HdrKind::DisplayReferred;
    let ev = kind.default_exposure_ev();
    for src in [2.0f32, 4.0, 8.0] {
        let soft =
            hdr::tonemap(&single(src, kind), &hdr::ToneLut::build(ev, ToneOp::Soft)).pixels[0].r();
        let clip =
            hdr::tonemap(&single(src, kind), &hdr::ToneLut::build(ev, ToneOp::Clip)).pixels[0].r();
        assert!(
            soft < clip,
            "{src} 的柔和曲線應低於硬截斷：{soft} vs {clip}"
        );
        assert!(soft < 255, "{src} 不該死白：{soft}");
    }
}

/// 中間調必須完全不受肩部影響，否則整體亮度會跑掉
#[test]
fn soft_curve_is_identity_in_midtones() {
    let kind = HdrKind::DisplayReferred;
    let ev = kind.default_exposure_ev();
    // scRGB 1.0 經 −1.5 EV 後約 0.354，仍在拐點 0.5 以下
    for src in [0.2f32, 0.5, 1.0, 1.4] {
        let soft =
            hdr::tonemap(&single(src, kind), &hdr::ToneLut::build(ev, ToneOp::Soft)).pixels[0].r();
        let clip =
            hdr::tonemap(&single(src, kind), &hdr::ToneLut::build(ev, ToneOp::Clip)).pixels[0].r();
        assert_eq!(soft, clip, "{src} 在拐點以下應與截斷完全相同");
    }
}

/// 預設曝光必須把紙白拉回 SDR 白，約 −1.5 EV
#[test]
fn display_referred_default_exposure_maps_paper_white() {
    let ev = HdrKind::DisplayReferred.default_exposure_ev();
    assert!(
        (-1.6..=-1.4).contains(&ev),
        "顯示參考的預設曝光應約 -1.5 EV，實際 {ev}"
    );
    assert_eq!(
        HdrKind::SceneReferred.default_exposure_ev(),
        0.0,
        "場景參考不該預設偏移曝光"
    );
}

/// 對照：0 EV（把 scRGB 1.0 當成白）會明顯偏亮——這正是修正前的症狀
#[test]
fn zero_ev_on_display_referred_content_is_visibly_too_bright() {
    let src = 0.305_2;
    let kind = HdrKind::DisplayReferred;
    let correct = hdr::tonemap(
        &single(src, kind),
        &hdr::ToneLut::build(kind.default_exposure_ev(), kind.default_tone_op()),
    )
    .pixels[0]
        .r();
    let too_bright =
        hdr::tonemap(&single(src, kind), &hdr::ToneLut::build(0.0, ToneOp::Clip)).pixels[0].r();
    let way_too_bright =
        hdr::tonemap(&single(src, kind), &hdr::ToneLut::build(0.0, ToneOp::Aces)).pixels[0].r();
    assert!(
        correct + 25 < too_bright && too_bright < way_too_bright,
        "亮度應為 修正後 < 0EV截斷 < 0EV的ACES：{correct} / {too_bright} / {way_too_bright}"
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

/// 曝光補償仍可用來救回極亮的高光
#[test]
fn exposure_recovers_clipped_highlights() {
    let kind = HdrKind::DisplayReferred;
    let src = 6.0f32; // 預設曝光下已超出範圍，會是白
    let at_default = hdr::tonemap(
        &single(src, kind),
        &hdr::ToneLut::build(kind.default_exposure_ev(), ToneOp::Clip),
    )
    .pixels[0]
        .r();
    assert_eq!(at_default, 255, "預設曝光下 6.0 應為白");

    // 再降 3 EV 就能看見其中的層次
    let down = hdr::tonemap(
        &single(src, kind),
        &hdr::ToneLut::build(kind.default_exposure_ev() - 3.0, ToneOp::Clip),
    )
    .pixels[0]
        .r();
    assert!(
        down < 240,
        "再降 3 EV 後應顯示出高光細節而非全白，實際 {down}"
    );
}
