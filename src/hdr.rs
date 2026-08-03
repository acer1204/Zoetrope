//! HDR / EXR 的浮點像素保存與色調映射。
//!
//! # 為什麼需要色調映射
//!
//! HDR 與 EXR 存的是真實光線強度（浮點、無上限），螢幕卻只有 0–255。
//! 把前者壓縮進後者的過程就是色調映射（tone mapping）；壓縮曲線不同，
//! 畫面亮暗與高光細節差很多。
//!
//! # 為什麼用查表
//!
//! 天真的做法是逐像素算 tonemap 再做 sRGB 編碼，但 `powf` 極慢——
//! 實測 4K EXR 要 1.3–2.5 秒，完全不能用。
//!
//! 關鍵洞見：對「逐通道」的運算子而言，
//! `g(x) = srgb_encode(tonemap(x × exposure))` 是一維函數；
//! 把來源存成 **f16** 之後 bit pattern 只有 65536 種，
//! 整條管線就塌縮成一次查表。實測 4K 降到 8–17ms，
//! 重建 LUT 只要約 1ms——拉曝光滑桿完全即時，且不需要 GPU shader，
//! wgpu 與 glow 兩個後端行為一致。
//!
//! # 為什麼還有第二條路徑
//!
//! 逐通道壓縮會**破壞色彩**：膚色的 R 遠大於 G、B，當 R 進入曲線的肩部
//! 被壓縮、而 G/B 還在拐點以下不動時，RGB 的比例就跑掉了，畫面往灰白靠。
//!
//! 量測 Windows 相簿證實它不是逐通道的：scRGB 的 R=4.0 在中性色塊被畫成
//! 236，在 (4.0, 1.0, 0.5) 卻畫成 255——同一個通道值、不同的輸出，代表
//! 映射會參考其他通道。實際比對三種模型後，相簿的行為對應到
//! **「曲線作用在亮度上，三通道同乘一個比例」**（見 tests/hdr_scrgb_match.rs）。
//!
//! 這條路徑跨通道，沒辦法塌縮成一維表，只好保留 `powf` 的部分改用
//! sRGB 編碼小表 + 多執行緒攤平。純灰階時兩條路徑的結果完全相同，
//! 所以先前用灰階階梯量到的曲線仍然成立。

use eframe::egui::ColorImage;

/// HDR 來源的參考基準。**這決定了正確的預設色調映射**，用錯會整張偏亮發灰。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HdrKind {
    /// **顯示參考**（scRGB）：Windows 遊戲列與 NVIDIA 的 HDR 截圖屬於此類。
    /// 1.0 就是 SDR 白（80 nits），超過的部分是 HDR 螢幕才顯示得出的高光。
    /// 正確做法是直接截斷到 0–1 再做 sRGB 編碼——這正是 Windows 相簿的行為，
    /// 套 ACES 反而會把中間調整體拉亮。
    DisplayReferred,
    /// **場景參考**（OpenEXR、Radiance HDR）：1.0 只是任意的中間值，
    /// 真實亮度可達數十倍，必須經色調映射才能塞進螢幕範圍。
    SceneReferred,
}

/// HDR 內容的「紙白」亮度（nits）。
///
/// scRGB 的 1.0 定義為 80 nits，但 HDR 內容實際把白色畫在更高的亮度上——
/// 遊戲與影片一般落在 200 nits 附近（BT.2408 建議 203）。轉成 SDR 時要把
/// **紙白**對應到螢幕白，而不是把 80 nits 當成白，否則整張會亮約 1.5 級。
///
/// 這個數值是量測 Windows 相簿的實際輸出反推得到的（見 tests/hdr_scrgb_match.rs）：
/// 相簿把 scRGB 1.0 畫成 161，等效倍率 ×0.354 → 80 / 0.354 ≈ 226 nits。
const HDR_PAPER_WHITE_NITS: f32 = 226.0;
/// scRGB 1.0 的定義亮度
const SCRGB_WHITE_NITS: f32 = 80.0;

impl HdrKind {
    pub fn default_tone_op(self) -> ToneOp {
        match self {
            HdrKind::DisplayReferred => ToneOp::Soft,
            HdrKind::SceneReferred => ToneOp::Aces,
        }
    }

    /// 預設曝光補償（EV）。
    ///
    /// 顯示參考的內容需要把紙白拉回 SDR 白（約 −1.5 EV）；
    /// 場景參考的內容沒有固定的白點基準，交給色調映射處理即可。
    pub fn default_exposure_ev(self) -> f32 {
        match self {
            HdrKind::DisplayReferred => {
                (SCRGB_WHITE_NITS / HDR_PAPER_WHITE_NITS).log2() // ≈ -1.5
            }
            HdrKind::SceneReferred => 0.0,
        }
    }
}

/// 以 f16 保存的 HDR 影像（RGBA，每像素 4 個 u16 bit pattern）。
/// f16 相對 f32 省一半記憶體，精度對顯示用途綽綽有餘，
/// 且正好是 GPU `Rgba16Float` 的原生佈局。
pub struct HdrImage {
    pub size: [usize; 2],
    /// RGBA 交錯，長度 = w × h × 4
    pub px: Vec<u16>,
    pub kind: HdrKind,
}

impl HdrImage {
    pub fn bytes(&self) -> usize {
        self.px.len() * 2
    }

    /// 2×2 箱形濾波減半（在浮點域做，避免先壓縮亮度再縮小造成高光錯誤）
    pub fn halved(&self) -> HdrImage {
        let [w, h] = self.size;
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut px = vec![0u16; nw * nh * 4];
        for y in 0..nh {
            let y0 = (y * 2).min(h - 1);
            let y1 = (y * 2 + 1).min(h - 1);
            for x in 0..nw {
                let x0 = (x * 2).min(w - 1);
                let x1 = (x * 2 + 1).min(w - 1);
                let idx = |xx: usize, yy: usize| (yy * w + xx) * 4;
                let (a, b, c, d) = (idx(x0, y0), idx(x1, y0), idx(x0, y1), idx(x1, y1));
                let o = (y * nw + x) * 4;
                for ch in 0..4 {
                    let sum = f16_to_f32(self.px[a + ch])
                        + f16_to_f32(self.px[b + ch])
                        + f16_to_f32(self.px[c + ch])
                        + f16_to_f32(self.px[d + ch]);
                    px[o + ch] = f32_to_f16(sum * 0.25);
                }
            }
        }
        HdrImage {
            size: [nw, nh],
            px,
            kind: self.kind,
        }
    }
}

#[inline]
pub fn f16_to_f32(bits: u16) -> f32 {
    half::f16::from_bits(bits).to_f32()
}

#[inline]
pub fn f32_to_f16(v: f32) -> u16 {
    half::f16::from_f32(v).to_bits()
}

/// 色調映射運算子。除了 `Soft` 之外都是「逐通道」形式，可以塌縮成一維查表。
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ToneOp {
    /// **柔和肩部**：中間調維持原樣，只在高光處滾降，且**保持色度**。
    ///
    /// 曲線本身是量測 Windows 相簿的灰階輸出擬合出來的——中間調完全是
    /// 恆等（所以亮度與相簿一致），超過拐點後接一段 Reinhard 形式的肩部。
    ///
    /// 關鍵在於它作用的對象是**亮度**而不是各個通道：算出 Rec.709 亮度 Y，
    /// 求 `Soft(Y)/Y`，三個通道同乘這個比例。RGB 的比例因此完全不變，
    /// 膚色不會在變亮的同時褪成灰白。超出 1.0 的通道直接截斷——相簿也是
    /// 這樣做的（實測 (4.0, 1.0, 0.5) 的 R 就是 255）。
    ///
    /// 顯示參考內容（HDR 截圖）的預設值。
    Soft,
    /// Narkowicz 的 ACES 近似式，電影感、對比較強
    Aces,
    /// Reinhard：x / (1 + x)，整體壓縮，中間調會偏暗
    Reinhard,
    /// 直接截斷，用來對照「完全不做滾降」的樣子
    Clip,
}

/// `Soft` 曲線的拐點：低於此值完全不動，高於此值開始滾降。
/// 0.5 是擬合相簿實測資料得到的（10 個取樣點誤差都在 3/255 以內）。
const SOFT_KNEE: f32 = 0.5;

/// Rec.709 / sRGB 的亮度係數
const LUMA_R: f32 = 0.2126;
const LUMA_G: f32 = 0.7152;
const LUMA_B: f32 = 0.0722;

impl ToneOp {
    pub fn name(self) -> &'static str {
        match self {
            ToneOp::Soft => "柔和",
            ToneOp::Aces => "ACES",
            ToneOp::Reinhard => "Reinhard",
            ToneOp::Clip => "截斷",
        }
    }

    /// 曲線是否作用在亮度上（保持 RGB 比例），而非逐通道套用。
    /// 逐通道會讓最亮的通道被壓得最多，於是彩色往灰白靠。
    pub fn preserves_chroma(self) -> bool {
        matches!(self, ToneOp::Soft)
    }

    #[inline]
    fn apply(self, x: f32) -> f32 {
        match self {
            ToneOp::Soft => {
                if x <= SOFT_KNEE {
                    x.max(0.0)
                } else {
                    // 拐點以上接 Reinhard 形式的肩部，漸近到 1.0
                    let over = x - SOFT_KNEE;
                    let room = 1.0 - SOFT_KNEE;
                    SOFT_KNEE + room * (over / (over + room))
                }
            }
            ToneOp::Aces => {
                // Narkowicz 2015 ACES filmic 近似
                const A: f32 = 2.51;
                const B: f32 = 0.03;
                const C: f32 = 2.43;
                const D: f32 = 0.59;
                const E: f32 = 0.14;
                ((x * (A * x + B)) / (x * (C * x + D) + E)).clamp(0.0, 1.0)
            }
            ToneOp::Reinhard => (x / (1.0 + x)).clamp(0.0, 1.0),
            ToneOp::Clip => x.clamp(0.0, 1.0),
        }
    }
}

/// 線性值 → sRGB 編碼（這一步的 powf 正是天真做法的效能瓶頸）
#[inline]
fn srgb_encode(x: f32) -> f32 {
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB 編碼小表的長度。保色度路徑的輸入是算出來的浮點值而非 f16，
/// 沒辦法用 bit pattern 當索引，只好改成均勻取樣。
///
/// sRGB 編碼在近黑處最陡（斜率 12.92），這裡的量化誤差最大：
/// 12.92 / 16384 × 255 ≈ 0.2 階，遠小於 1，不會造成色帶。
const SRGB_TAB: usize = 16384;

/// 來源清理：NaN、負值、無限大一律視為 0，避免壞資料變成雜訊。
/// scRGB 允許負值表示超出 sRGB 色域的顏色，我們沒有做色域映射，就當黑處理。
#[inline]
fn sanitize(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 {
        v
    } else {
        0.0
    }
}

/// 把「曝光 → 色調映射 → sRGB 編碼」預先算成查表。
enum LutKind {
    /// 逐通道運算子：整條管線塌縮成一張表，索引是 f16 的 bit pattern
    PerChannel { color: Box<[u8; 65536]> },
    /// 保色度運算子：跨通道，只能把 sRGB 編碼查表化
    Chroma {
        gain: f32,
        op: ToneOp,
        srgb: Box<[u8; SRGB_TAB]>,
    },
}

pub struct ToneLut {
    kind: LutKind,
    /// alpha 通道：只做 clamp，不套 tonemap 也不做 gamma
    alpha: Box<[u8; 65536]>,
}

impl ToneLut {
    pub fn build(exposure_ev: f32, op: ToneOp) -> Self {
        let gain = 2f32.powf(exposure_ev);
        let mut alpha = Box::new([0u8; 65536]);
        for bits in 0..=u16::MAX {
            let v = sanitize(f16_to_f32(bits));
            alpha[bits as usize] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }

        let kind = if op.preserves_chroma() {
            let mut srgb = Box::new([0u8; SRGB_TAB]);
            for (i, slot) in srgb.iter_mut().enumerate() {
                let x = i as f32 / (SRGB_TAB - 1) as f32;
                *slot = (srgb_encode(x) * 255.0 + 0.5) as u8;
            }
            LutKind::Chroma { gain, op, srgb }
        } else {
            let mut color = Box::new([0u8; 65536]);
            for bits in 0..=u16::MAX {
                let v = sanitize(f16_to_f32(bits));
                let c = srgb_encode(op.apply(v * gain));
                color[bits as usize] = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            LutKind::PerChannel { color }
        };
        Self { kind, alpha }
    }

    /// 單一通道的查表值。只有逐通道路徑才有意義——保色度路徑的輸出
    /// 取決於同一像素的其他通道，無法單獨查表，這裡回傳中性灰的結果。
    #[inline]
    pub fn color(&self, bits: u16) -> u8 {
        match &self.kind {
            LutKind::PerChannel { color } => color[bits as usize],
            LutKind::Chroma { gain, op, srgb } => {
                // R=G=B 時亮度就等於該值，比例為 1，等同直接套曲線
                encode(srgb, op.apply(sanitize(f16_to_f32(bits)) * gain))
            }
        }
    }

    #[inline]
    pub fn alpha(&self, bits: u16) -> u8 {
        self.alpha[bits as usize]
    }
}

/// 線性值 → 8-bit sRGB。`f32 as usize` 在 Rust 是飽和轉換：
/// 負值變 0、過大值變 usize::MAX，所以只需要一次上界夾取。
#[inline]
fn encode(tab: &[u8; SRGB_TAB], v: f32) -> u8 {
    let i = (v * (SRGB_TAB - 1) as f32) as usize;
    tab[i.min(SRGB_TAB - 1)]
}

/// 低於這個像素數就不開執行緒——縮圖與 mip 尾端的圖太小，
/// 建立執行緒的成本反而超過計算本身。
const PARALLEL_MIN_PX: usize = 1 << 20;

/// 套用查表產生可顯示的 8-bit 影像
pub fn tonemap(hdr: &HdrImage, lut: &ToneLut) -> ColorImage {
    let [w, h] = hdr.size;
    let mut out = vec![0u8; w * h * 4];

    // 保色度路徑跨通道，沒辦法只靠查表，成本高出數倍——切列平行處理補回來。
    let threads = if w * h >= PARALLEL_MIN_PX && matches!(lut.kind, LutKind::Chroma { .. }) {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(8)
    } else {
        1
    };

    if threads <= 1 {
        map_span(&mut out, &hdr.px, lut);
    } else {
        let span = h.div_ceil(threads) * w * 4;
        std::thread::scope(|s| {
            for (o, i) in out.chunks_mut(span).zip(hdr.px.chunks(span)) {
                s.spawn(move || map_span(o, i, lut));
            }
        });
    }
    ColorImage::from_rgba_unmultiplied([w, h], &out)
}

fn map_span(out: &mut [u8], px: &[u16], lut: &ToneLut) {
    match &lut.kind {
        LutKind::PerChannel { color } => {
            for (o, s) in out.chunks_exact_mut(4).zip(px.chunks_exact(4)) {
                o[0] = color[s[0] as usize];
                o[1] = color[s[1] as usize];
                o[2] = color[s[2] as usize];
                o[3] = lut.alpha[s[3] as usize];
            }
        }
        LutKind::Chroma { gain, op, srgb } => {
            for (o, s) in out.chunks_exact_mut(4).zip(px.chunks_exact(4)) {
                let r = sanitize(f16_to_f32(s[0])) * gain;
                let g = sanitize(f16_to_f32(s[1])) * gain;
                let b = sanitize(f16_to_f32(s[2])) * gain;
                // 曲線只作用在亮度上，三通道同乘同一個比例 → RGB 比例不變
                let y = LUMA_R * r + LUMA_G * g + LUMA_B * b;
                let ratio = if y > 0.0 { op.apply(y) / y } else { 0.0 };
                o[0] = encode(srgb, r * ratio);
                o[1] = encode(srgb, g * ratio);
                o[2] = encode(srgb, b * ratio);
                o[3] = lut.alpha[s[3] as usize];
            }
        }
    }
}

/// DynamicImage 的浮點變體 → HdrImage（EXR/Radiance 皆為場景參考）。
/// 非浮點格式回傳 None。
pub fn from_dynamic(img: &image::DynamicImage) -> Option<HdrImage> {
    use image::DynamicImage as D;
    let (w, h) = match img {
        D::ImageRgb32F(b) => (b.width() as usize, b.height() as usize),
        D::ImageRgba32F(b) => (b.width() as usize, b.height() as usize),
        _ => return None,
    };
    let mut px = Vec::with_capacity(w * h * 4);
    match img {
        D::ImageRgb32F(b) => {
            for p in b.pixels() {
                px.push(f32_to_f16(p.0[0]));
                px.push(f32_to_f16(p.0[1]));
                px.push(f32_to_f16(p.0[2]));
                px.push(f32_to_f16(1.0));
            }
        }
        D::ImageRgba32F(b) => {
            for p in b.pixels() {
                for c in 0..4 {
                    px.push(f32_to_f16(p.0[c]));
                }
            }
        }
        _ => unreachable!(),
    }
    Some(HdrImage {
        size: [w, h],
        px,
        kind: HdrKind::SceneReferred,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(px: &[[f32; 4]], w: usize) -> HdrImage {
        let mut v = Vec::new();
        for p in px {
            for c in p {
                v.push(f32_to_f16(*c));
            }
        }
        HdrImage {
            size: [w, px.len() / w],
            px: v,
            kind: HdrKind::SceneReferred,
        }
    }

    #[test]
    fn f16_roundtrip_keeps_display_precision() {
        for v in [0.0f32, 0.5, 1.0, 2.0, 100.0, 0.001] {
            let back = f16_to_f32(f32_to_f16(v));
            assert!(
                (back - v).abs() <= v.abs() * 0.001 + 1e-4,
                "{v} → {back} 誤差過大"
            );
        }
    }

    #[test]
    fn aces_compresses_highlights_without_clipping_everything() {
        let op = ToneOp::Aces;
        // 亮度遠超 1.0 的值仍應落在 0..1 且保持單調遞增
        let a = op.apply(1.0);
        let b = op.apply(4.0);
        let c = op.apply(100.0);
        assert!(a < b && b <= c, "應單調遞增：{a} {b} {c}");
        assert!(c <= 1.0);
        // 關鍵：高光沒有全部糊成同一個值（截斷法會 b == c == 1.0）
        assert!(b < 0.999, "4.0 不該直接飽和成白：{b}");
        // 對照組：截斷法在 1.0 以上就全白
        assert_eq!(ToneOp::Clip.apply(4.0), 1.0);
    }

    #[test]
    fn lut_matches_direct_computation() {
        let lut = ToneLut::build(0.0, ToneOp::Aces);
        for v in [0.0f32, 0.18, 0.5, 1.0, 3.0, 20.0] {
            let bits = f32_to_f16(v);
            let direct = srgb_encode(ToneOp::Aces.apply(f16_to_f32(bits)));
            let expect = (direct.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            assert_eq!(lut.color(bits), expect, "v={v} 查表與直接計算不符");
        }
    }

    #[test]
    fn exposure_brightens_monotonically() {
        let bits = f32_to_f16(0.18); // 中灰
        let dark = ToneLut::build(-2.0, ToneOp::Aces).color(bits);
        let mid = ToneLut::build(0.0, ToneOp::Aces).color(bits);
        let bright = ToneLut::build(2.0, ToneOp::Aces).color(bits);
        assert!(dark < mid && mid < bright, "{dark} {mid} {bright}");
    }

    #[test]
    fn tonemap_output_shape_and_alpha() {
        let hdr = make(
            &[
                [0.0, 0.0, 0.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [8.0, 4.0, 2.0, 0.5],
                [0.2, 0.2, 0.2, 1.0],
            ],
            2,
        );
        let lut = ToneLut::build(0.0, ToneOp::Aces);
        let img = tonemap(&hdr, &lut);
        assert_eq!(img.size, [2, 2]);
        assert_eq!(img.pixels.len(), 4);
        // 黑仍是黑
        assert_eq!(img.pixels[0].r(), 0);
        // 半透明像素的 alpha 要保留（約 128）
        let a = img.pixels[2].a();
        assert!((120..=136).contains(&a), "alpha 應約 128，實際 {a}");
    }

    #[test]
    fn negative_and_nan_are_treated_as_black() {
        let lut = ToneLut::build(0.0, ToneOp::Aces);
        assert_eq!(lut.color(f32_to_f16(-5.0)), 0);
        assert_eq!(lut.color(f32_to_f16(f32::NAN)), 0);
        assert_eq!(lut.color(f32_to_f16(f32::INFINITY)), 0);
    }

    #[test]
    fn halving_averages_in_linear_space() {
        // 一個很亮、三個全黑 → 平均應為 2.0，而非先壓縮再平均的結果
        let hdr = make(
            &[
                [8.0, 8.0, 8.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            2,
        );
        let h = hdr.halved();
        assert_eq!(h.size, [1, 1]);
        let v = f16_to_f32(h.px[0]);
        assert!((v - 2.0).abs() < 0.01, "應為 2.0，實際 {v}");
    }

    #[test]
    fn dynamic_image_conversion() {
        let buf = image::Rgb32FImage::from_fn(3, 2, |x, _| image::Rgb([x as f32, 0.5, 2.0]));
        let hdr = from_dynamic(&image::DynamicImage::ImageRgb32F(buf)).expect("應可轉換");
        assert_eq!(hdr.size, [3, 2]);
        assert_eq!(hdr.px.len(), 3 * 2 * 4);
        assert_eq!(f16_to_f32(hdr.px[3]), 1.0, "RGB 來源的 alpha 應補 1.0");
        // 非浮點格式不該被誤判
        assert!(from_dynamic(&image::DynamicImage::new_rgba8(2, 2)).is_none());
    }
}
