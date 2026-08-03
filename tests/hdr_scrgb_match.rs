//! 驗證「顯示參考」HDR（scRGB，Windows/NVIDIA 的 HDR 截圖）的轉換
//! 與 Windows 相簿一致。
//!
//! 背景：scRGB 的 1.0 只有 80 nits，而 HDR 內容的「紙白」實際上落在
//! 200 nits 附近。轉成 SDR 時要把**紙白**對應到螢幕白，否則整張會偏亮約
//! 1.5 級；曲線要用保色度的柔和肩部，ACES 是給場景參考的 EXR 用的。
//!
//! 期望值是實測 Windows 相簿得來的：產生已知 scRGB 數值的階梯圖與色塊圖，
//! 用相簿開啟後從畫面讀回每一塊的顏色。最關鍵的一點是相簿把 scRGB 1.0
//! 畫成 161（而非 255），由此反推出紙白約 226 nits。
//!
//! # 這組期望值的適用範圍
//!
//! 全部來自**單一台螢幕上的單次量測**。相簿很可能是依
//! `DXGI_OUTPUT_DESC1::MaxLuminance` 決定輸出白階的，所以 226 nits 這個數字
//! 應該視為「這台螢幕的校準值」而非通用常數——換一台機器重跑，整組可能會紅。
//! 量測環境：Windows 11、HDR 關閉、SDR 桌面。

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

/// 相簿實測的全部彩色色塊。灰階量不出色彩行為——中性色上每一種候選做法的
/// 結果都完全相同（冪平均滿足 `M(v,v,v) = v`），所以最早那版才會通過所有
/// 灰階測試卻把膚色映射成灰白。
///
/// 這組數據來自兩張獨立的色塊圖，用相簿開啟後從畫面讀回
/// （見 scratchpad 的 colorwedge / colorwedge3 / readruns2）。第二張是
/// **100% 顯示**、色塊之間加了灰色間隔，讀回的每一塊 spread 都是 0，
/// 所以不存在縮放濾波把鄰塊顏色滲進來的可能。兩張圖重疊的三塊完全一致。
///
/// 決定性的觀察在前兩筆：scRGB 的 R=4.0 在中性色塊被畫成 236，
/// 在 (4.0, 1.0, 0.5) 卻是 255。同一個通道值有兩種輸出，
/// 證明相簿的映射會參考其他通道，不可能是逐通道的。
const PHOTOS: &[(f32, f32, f32, [u8; 3])] = &[
    (4.0, 4.0, 4.0, [236, 236, 236]), // 中性對照：重現灰階階梯的量測值
    (4.0, 1.0, 0.5, [255, 161, 118]), // 同一個 R=4.0，輸出卻是 255
    (2.0, 2.0, 2.0, [213, 213, 213]), // 中性對照
    (0.5, 0.5, 0.5, [118, 118, 118]), // 中性對照（第二張圖的間隔色）
    (2.0, 1.0, 0.7, [220, 161, 137]), // 亮膚色
    (3.0, 1.8, 1.4, [255, 205, 184]), // 更亮的膚色
    (2.5, 1.5, 1.1, [242, 193, 168]), // 典型亮部膚色
    (1.5, 0.9, 0.7, [193, 154, 137]), // 中間調膚色
    (1.0, 0.5, 0.25, [161, 118, 85]), // 拐點以下，不受曲線影響
    (3.0, 3.0, 1.0, [241, 241, 150]), // 以下四塊專門用來分辨驅動量
    (1.5, 3.0, 1.5, [182, 245, 182]),
    (4.0, 4.0, 2.0, [246, 246, 185]),
    (2.0, 3.0, 1.5, [204, 243, 180]),
    (6.0, 0.3, 0.3, [255, 93, 93]),   // 飽和紅高光
    (0.3, 0.3, 6.0, [92, 93, 255]),   // 飽和藍高光
    (8.0, 2.0, 1.0, [255, 197, 146]), // 極亮暖色
    (0.3, 6.0, 0.3, [100, 255, 91]),  // 飽和綠高光——見下方的不對稱說明
];

#[test]
fn matches_windows_photos_on_colour() {
    let mut worst = 0u8;
    let mut sse = 0f64;
    for &(r, g, b, expect) in PHOTOS {
        let got = map_default(r, g, b);
        for c in 0..3 {
            let d = got[c].abs_diff(expect[c]);
            worst = worst.max(d);
            sse += (d as f64) * (d as f64);
        }
        assert!(
            (0..3).all(|c| got[c].abs_diff(expect[c]) <= 9),
            "來源 ({r}, {g}, {b}) 應輸出約 {expect:?}（相簿實測值），實際 {got:?}"
        );
    }
    let rms = (sse / (PHOTOS.len() * 3) as f64).sqrt();
    eprintln!("彩色色塊：最大偏差 {worst}/255，rms {rms:.2}");
    assert!(rms < 4.0, "整體 rms 不該超過 4，實際 {rms:.2}");
}

/// 剩下的殘差不是曲線沒調好，而是**任何等比模型都到不了的下限**。
///
/// (0.3, 6.0, 0.3) 的 R 與 B 在檔案裡是位元相同的值，相簿卻畫成 R=100、
/// B=91。任何「三通道同乘一個純量」的運算子——逐通道、maxRGB、亮度等比、
/// 冪平均——對相同的輸入通道必然給出相同的輸出。9 階的差距不可能來自 f16
/// 捨入或 8-bit 量化，代表相簿管線裡有一個**非對角**的轉換（色彩空間轉換、
/// 色域映射，或在 ICtCp 之類的空間裡壓縮）。那是什麼，目前沒有查出來。
///
/// 這條測試把「我們已經打到等比模型的下限」這件事鎖起來：整組最大偏差
/// 不該超過那個不對稱本身的大小。哪天有人想再壓低誤差，看到這裡就會知道
/// 該去找那個非對角轉換，而不是繼續調曲線參數。
#[test]
fn residual_is_bounded_by_photos_own_channel_asymmetry() {
    let asym = {
        let m = PHOTOS
            .iter()
            .find(|p| (p.0, p.1, p.2) == (0.3, 6.0, 0.3))
            .expect("綠色高光色塊");
        m.3[0].abs_diff(m.3[2]) // 相同輸入通道，相簿卻給出不同輸出
    };
    assert_eq!(asym, 9, "相簿的非對角殘差應為 9 階");

    let worst = PHOTOS
        .iter()
        .flat_map(|&(r, g, b, e)| {
            let got = map_default(r, g, b);
            (0..3)
                .map(move |c| got[c].abs_diff(e[c]))
                .collect::<Vec<_>>()
        })
        .max()
        .unwrap();
    assert!(
        worst <= asym,
        "最大偏差 {worst} 已超過相簿自身的非對角不對稱 {asym}——\
         代表等比模型這一層還有調整空間，不能只怪非對角轉換"
    );
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

/// 驅動量必須是**未加權**的：誤差若隨通道權重放大，就是選錯範數的指紋。
///
/// 這是當初用 Rec.709 亮度時被抓包的方式——綠色主導的色塊差 32 階
/// （G 權重 0.7152）、紅色主導只差 2（R 0.2126）、藍色主導差 1（B 0.0722）。
/// 三個通道對稱之後，同一亮度的紅／綠／藍高光偏差應該落在同一個量級。
#[test]
fn driver_treats_channels_symmetrically() {
    let err = |r: f32, g: f32, b: f32, e: [u8; 3]| {
        let got = map_default(r, g, b);
        (0..3).map(|c| got[c].abs_diff(e[c])).max().unwrap()
    };
    let red = err(6.0, 0.3, 0.3, [255, 93, 93]);
    let green = err(0.3, 6.0, 0.3, [100, 255, 91]);
    let blue = err(0.3, 0.3, 6.0, [92, 93, 255]);
    // 綠色那一塊的殘差來自相簿自身的非對角不對稱（見上一條測試），
    // 但不該再出現「綠比藍差一個數量級」這種加權範數的特徵
    assert!(
        green <= blue + 9 && red <= blue + 9,
        "三個原色的殘差應在同一量級：紅 {red} / 綠 {green} / 藍 {blue}"
    );
}

/// 依內容挑曲線：在 HDR 桌面上擷取的截圖，整張可能根本沒有 HDR。
///
/// 這組數值取自兩張真實的 NVIDIA 桌面截圖與一張遊戲截圖：
///
/// | 來源 | p99 | 峰值 | 該用的曲線 |
/// |---|---|---|---|
/// | 桌面截圖（純 UI） | 2.52 | **3.00** | 截斷——沒有高光可壓 |
/// | 桌面截圖（含 HDR 視窗） | 7.37 | 13.68 | 柔和 |
/// | 遊戲截圖 | 9.79 | 12.72 | 柔和 |
///
/// 第一張的峰值只有 3.0，剛好貼著螢幕白階（2.83 = 226 nits）。對它套肩部
/// 曲線會把 UI 的底色與文字壓進很窄的範圍——實測兩個原本相差 17 階的灰只
/// 剩 8 階，看起來就是一片過曝。
fn peaky(peak: f32, fraction: f64, w: usize, h: usize) -> HdrImage {
    // 底色固定在螢幕白階附近，只有指定比例的像素拉到 peak
    let n = w * h;
    let bright = ((n as f64) * fraction) as usize;
    let mut px = Vec::with_capacity(n * 4);
    for i in 0..n {
        let v = if i < bright { peak } else { 2.0 };
        for _ in 0..3 {
            px.push(hdr::f32_to_f16(v));
        }
        px.push(hdr::f32_to_f16(1.0));
    }
    HdrImage {
        size: [w, h],
        px,
        kind: HdrKind::DisplayReferred,
    }
}

#[test]
fn picks_clip_when_the_file_has_no_hdr_headroom() {
    // 純 SDR 桌面截圖：峰值 3.0，貼著白階
    assert_eq!(
        peaky(3.0, 0.0, 200, 100).recommended_tone_op(),
        ToneOp::Clip
    );
    // 含 HDR 內容的桌面截圖：峰值 13.68，且佔比夠大
    assert_eq!(
        peaky(13.68, 0.05, 200, 100).recommended_tone_op(),
        ToneOp::Soft
    );
    // 遊戲截圖：大量像素在高光區
    assert_eq!(
        peaky(12.72, 0.3, 200, 100).recommended_tone_op(),
        ToneOp::Soft
    );
}

#[test]
fn a_few_stray_bright_pixels_do_not_flip_the_curve() {
    // 十萬分之一的過亮雜訊像素不該讓整張圖改走另一條曲線
    assert_eq!(
        peaky(60.0, 0.000_01, 400, 250).recommended_tone_op(),
        ToneOp::Clip
    );
    // 但 1% 就確實是內容的一部分了
    assert_eq!(
        peaky(60.0, 0.01, 400, 250).recommended_tone_op(),
        ToneOp::Soft
    );
}

/// 場景參考的 EXR 沒有固定白點基準，這個判斷對它沒有意義，必須維持 ACES
#[test]
fn scene_referred_is_unaffected_by_content_detection() {
    let dim = HdrImage {
        size: [8, 8],
        px: vec![hdr::f32_to_f16(0.05); 8 * 8 * 4],
        kind: HdrKind::SceneReferred,
    };
    assert_eq!(dim.recommended_tone_op(), ToneOp::Aces);
}

/// 這是「文字與底色糊在一起」的數字版：桌面截圖裡最主要的兩個 UI 灰階，
/// 在截斷之下必須完整保留原本的階差
#[test]
fn clip_reproduces_desktop_ui_contrast_that_the_shoulder_crushes() {
    let kind = HdrKind::DisplayReferred;
    let ev = kind.default_exposure_ev();
    // 實測該檔案中佔比最高的兩個中性亮階（44.4% 與 43.2%）
    let (a, b) = (2.0f32, 2.378);

    let sep = |op: ToneOp| {
        let lut = hdr::ToneLut::build(ev, op);
        let g = |v: f32| hdr::tonemap(&single(v, kind), &lut).pixels[0].r();
        g(b).abs_diff(g(a))
    };

    assert!(
        sep(ToneOp::Clip) >= 15,
        "截斷應保留約 17 階的差距，實際 {}",
        sep(ToneOp::Clip)
    );
    assert!(
        sep(ToneOp::Soft) <= 10,
        "肩部確實會壓掉對比（這正是問題所在），實際 {}",
        sep(ToneOp::Soft)
    );
}

/// 保色度路徑要能安全處理壞資料與全黑像素（除以驅動量時的邊界）
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
