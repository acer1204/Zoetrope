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

fn rgb(r: f32, g: f32, b: f32, kind: HdrKind) -> HdrImage {
    HdrImage {
        size: [1, 1],
        px: vec![
            hdr::f32_to_f16(r),
            hdr::f32_to_f16(g),
            hdr::f32_to_f16(b),
            hdr::f32_to_f16(1.0),
        ],
        kind,
    }
}

/// 用預設設定（-1.5 EV + 柔和）映射一個顏色，回傳 8-bit sRGB
fn map_default(r: f32, g: f32, b: f32) -> [u8; 3] {
    let kind = HdrKind::DisplayReferred;
    let lut = hdr::ToneLut::build(kind.default_exposure_ev(), kind.default_tone_op());
    let p = hdr::tonemap(&rgb(r, g, b, kind), &lut).pixels[0];
    [p.r(), p.g(), p.b()]
}

/// sRGB 8-bit → 線性，用來檢查色度而不是編碼後的數值
fn to_linear(v: u8) -> f32 {
    let x = v as f32 / 255.0;
    if x <= 0.040_45 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// 飽和度：(max - min) / max，在線性空間量
fn saturation(c: [u8; 3]) -> f32 {
    let l = [to_linear(c[0]), to_linear(c[1]), to_linear(c[2])];
    let max = l[0].max(l[1]).max(l[2]);
    let min = l[0].min(l[1]).min(l[2]);
    if max <= 0.0 {
        0.0
    } else {
        (max - min) / max
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

/// **彩色**回歸測試。灰階量不出色彩行為——中性色上「逐通道」與「亮度等比」
/// 的結果完全相同，所以前一版才會通過所有灰階測試卻把膚色映射成灰白。
///
/// 期望值同樣是實測來的：產生一張已知 scRGB 的彩色色塊圖，用相簿開啟，
/// 再從畫面讀回每一塊的顏色（見 scratchpad 的 colorwedge/readcolorruns）。
///
/// 決定性的觀察在前兩筆：scRGB 的 R=4.0 在中性色塊被畫成 236，
/// 在 (4.0, 1.0, 0.5) 卻是 255。同一個通道值有兩種輸出，
/// 證明相簿的映射會參考其他通道，不可能是逐通道的。
#[test]
fn matches_windows_photos_on_colour() {
    // (scRGB R, G, B, 相簿實際畫出的 sRGB)
    let cases = [
        (4.0f32, 4.0, 4.0, [236u8, 236, 236]), // 中性對照：重現灰階階梯的量測值
        (4.0, 1.0, 0.5, [255, 161, 118]),      // 同一個 R=4.0，輸出卻是 255
        (2.0, 2.0, 2.0, [213, 213, 213]),      // 中性對照
        (2.0, 1.0, 0.7, [220, 161, 137]),      // 亮膚色
        (3.0, 1.8, 1.4, [255, 205, 184]),      // 更亮的膚色
        (2.5, 1.5, 1.1, [242, 193, 168]),      // 典型亮部膚色
        (1.5, 0.9, 0.7, [193, 154, 137]),      // 中間調膚色
        (1.0, 0.5, 0.25, [161, 118, 85]),      // 拐點以下，不受曲線影響
        (6.0, 0.3, 0.3, [255, 93, 93]),        // 飽和紅高光
        (0.3, 0.3, 6.0, [92, 93, 255]),        // 飽和藍高光
    ];

    let mut worst = 0u8;
    for (r, g, b, expect) in cases {
        let got = map_default(r, g, b);
        for c in 0..3 {
            worst = worst.max(got[c].abs_diff(expect[c]));
        }
        assert!(
            (0..3).all(|c| got[c].abs_diff(expect[c]) <= 6),
            "來源 ({r}, {g}, {b}) 應輸出約 {expect:?}（相簿實測值），實際 {got:?}"
        );
    }
    eprintln!("彩色色塊與相簿的最大偏差：{worst}/255");
}

/// 這一條直指使用者回報的症狀：「膚色偏白，相簿看起來紅潤」。
///
/// 逐通道壓縮會把最亮的通道壓得最多。膚色的 R 遠大於 G、B，於是 R 被拉低、
/// G/B 幾乎不動，比例跑掉 → 往灰白靠。亮度等比則三通道同乘一個數，比例不變。
#[test]
fn soft_keeps_skin_saturated_where_per_channel_washes_it_out() {
    let (r, g, b) = (3.0f32, 1.8, 1.4); // 亮部膚色
    let kind = HdrKind::DisplayReferred;
    let ev = kind.default_exposure_ev();

    let soft = map_default(r, g, b);
    let per_channel = {
        let lut = hdr::ToneLut::build(ev, ToneOp::Aces); // ACES 是逐通道的
        let p = hdr::tonemap(&rgb(r, g, b, kind), &lut).pixels[0];
        [p.r(), p.g(), p.b()]
    };

    assert!(
        saturation(soft) > saturation(per_channel) * 1.2,
        "保色度的飽和度應明顯高於逐通道：{:.3} vs {:.3}（{soft:?} / {per_channel:?}）",
        saturation(soft),
        saturation(per_channel)
    );

    // 更直接的說法：紅相對綠的強度。逐通道把 R 壓掉，於是膚色褪成灰白。
    let ratio = |c: [u8; 3]| to_linear(c[0]) / to_linear(c[1]);
    let want = r / g; // 來源的 R:G
    assert!(
        (ratio(soft) - want).abs() < 0.05,
        "保色度應維持來源的 R:G = {want:.3}，實際 {:.3}",
        ratio(soft)
    );
    assert!(
        ratio(per_channel) < want - 0.15,
        "逐通道應明顯拉低 R:G（這就是偏白的成因），實際 {:.3}",
        ratio(per_channel)
    );
}

/// 沒有任何通道溢出時，線性 RGB 的比例必須原封不動——這是「保色度」的定義
#[test]
fn soft_preserves_linear_ratios_when_nothing_clips() {
    // (3.0, 1.8, 1.4) 經 -1.5 EV 後亮度 0.716 已進入肩部，
    // 但最大通道映射後仍在 1.0 以下，所以不會被截斷
    let (r, g, b) = (3.0f32, 1.8, 1.4);
    let out = map_default(r, g, b);
    assert!(out.iter().all(|&c| c < 255), "這組不該有通道到頂：{out:?}");

    let lin = [to_linear(out[0]), to_linear(out[1]), to_linear(out[2])];
    let src = [r, g, b];
    for c in 1..3 {
        let want = src[c] / src[0];
        let have = lin[c] / lin[0];
        assert!(
            (want - have).abs() < 0.02,
            "通道 {c} 的比例應維持 {want:.3}，實際 {have:.3}"
        );
    }
}

/// 已知落差：極端飽和的高光（單一通道 6~8、其餘約 0.3）。
///
/// 相簿在通道被截斷之後還會補回一部分飽和度，我們沒有跟進——實測的補償量
/// 換算成線性後落在 0.06~0.25 之間，並不是一個一致的常數，硬擬合只會過擬合
/// 這十幾個樣本。這條測試把現況的落差鎖起來，避免哪天無意間變得更差。
#[test]
fn known_gap_on_extreme_saturated_highlights() {
    // (scRGB, 相簿實測, 我們目前的偏差上限)
    let cases = [
        (8.0f32, 2.0, 1.0, [255u8, 197, 146], 14u8),
        (0.3, 6.0, 0.3, [100, 255, 91], 34),
    ];
    for (r, g, b, photos, limit) in cases {
        let got = map_default(r, g, b);
        let diff = (0..3).map(|c| got[c].abs_diff(photos[c])).max().unwrap();
        assert!(
            diff <= limit,
            "({r}, {g}, {b}) 與相簿的落差不該超過 {limit}，實際 {diff}（{got:?} vs {photos:?}）"
        );
        // 被截斷的通道本身仍必須對得上
        let ch = if g > r { 1 } else { 0 };
        assert_eq!(got[ch], photos[ch], "主通道應與相簿一致：{got:?}");
    }
}

/// 保色度路徑要能安全處理壞資料與全黑像素（除以亮度時的邊界）
#[test]
fn chroma_path_handles_black_and_bad_pixels() {
    assert_eq!(map_default(0.0, 0.0, 0.0), [0, 0, 0], "全黑應維持全黑");
    assert_eq!(map_default(-2.0, -2.0, -2.0), [0, 0, 0], "負值視為黑");
    assert_eq!(map_default(f32::NAN, f32::NAN, f32::NAN), [0, 0, 0]);
    // 單一通道為 NaN 不該污染其他通道
    let mixed = map_default(f32::NAN, 1.0, 0.5);
    assert_eq!(mixed[0], 0, "NaN 通道應為 0，實際 {mixed:?}");
    assert!(mixed[1] > 0 && mixed[2] > 0, "其他通道應正常：{mixed:?}");
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
