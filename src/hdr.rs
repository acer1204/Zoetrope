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

impl HdrKind {
    pub fn default_tone_op(self) -> ToneOp {
        match self {
            HdrKind::DisplayReferred => ToneOp::Clip,
            HdrKind::SceneReferred => ToneOp::Aces,
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

/// 色調映射運算子。全部都是「逐通道」形式，才能塌縮成一維查表——
/// 像 Khronos PBR Neutral 那種需要 min/max(rgb) 的運算子無法用 LUT。
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ToneOp {
    /// Narkowicz 的 ACES 近似式，電影感、高光滾降自然（預設）
    Aces,
    /// Reinhard：x / (1 + x)，溫和、保留較多中間調
    Reinhard,
    /// 直接截斷，用來對照「未做色調映射」的樣子
    Clip,
}

impl ToneOp {
    pub fn name(self) -> &'static str {
        match self {
            ToneOp::Aces => "ACES",
            ToneOp::Reinhard => "Reinhard",
            ToneOp::Clip => "截斷",
        }
    }

    #[inline]
    fn apply(self, x: f32) -> f32 {
        match self {
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

/// 把「曝光 → 色調映射 → sRGB 編碼」整條管線預先算成查表。
/// 索引是 f16 的 bit pattern，因此涵蓋所有可能的來源值。
pub struct ToneLut {
    /// 顏色通道：含曝光、tonemap 與 gamma
    color: Box<[u8; 65536]>,
    /// alpha 通道：只做 clamp，不套 tonemap 也不做 gamma
    alpha: Box<[u8; 65536]>,
}

impl ToneLut {
    pub fn build(exposure_ev: f32, op: ToneOp) -> Self {
        let gain = 2f32.powf(exposure_ev);
        let mut color = Box::new([0u8; 65536]);
        let mut alpha = Box::new([0u8; 65536]);
        for bits in 0..=u16::MAX {
            let v = f16_to_f32(bits);
            // NaN / 負值一律視為 0，避免壞資料造成雜訊
            let v = if v.is_finite() && v > 0.0 { v } else { 0.0 };
            let c = srgb_encode(op.apply(v * gain));
            color[bits as usize] = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            alpha[bits as usize] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        Self { color, alpha }
    }

    #[inline]
    pub fn color(&self, bits: u16) -> u8 {
        self.color[bits as usize]
    }

    #[inline]
    pub fn alpha(&self, bits: u16) -> u8 {
        self.alpha[bits as usize]
    }
}

/// 套用查表產生可顯示的 8-bit 影像
pub fn tonemap(hdr: &HdrImage, lut: &ToneLut) -> ColorImage {
    let [w, h] = hdr.size;
    let mut out = vec![0u8; w * h * 4];
    for (o, s) in out.chunks_exact_mut(4).zip(hdr.px.chunks_exact(4)) {
        o[0] = lut.color(s[0]);
        o[1] = lut.color(s[1]);
        o[2] = lut.color(s[2]);
        o[3] = lut.alpha(s[3]);
    }
    ColorImage::from_rgba_unmultiplied([w, h], &out)
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
