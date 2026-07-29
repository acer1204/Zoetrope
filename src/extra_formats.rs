//! image crate 不支援的格式：JPEG XL、AVIF、HEIC/HEIF、相機 RAW。
//! 全部使用純 Rust 解碼器，不需要任何 C 函式庫。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use image::{Rgba, RgbaImage};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExtraFormat {
    JpegXl,
    Avif,
    Heif,
    Raw,
}

impl ExtraFormat {
    pub fn name(self) -> &'static str {
        match self {
            ExtraFormat::JpegXl => "JPEG XL",
            ExtraFormat::Avif => "AVIF",
            ExtraFormat::Heif => "HEIC/HEIF",
            ExtraFormat::Raw => "RAW",
        }
    }
}

/// 相機 RAW 副檔名（rawloader 支援的主要機種）。
/// RAW 幾乎都是 TIFF 變體，無法只靠檔頭區分，因此以副檔名判斷。
pub const RAW_EXTS: &[&str] = &[
    "cr2", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf", "rw2", "pef", "srw",
    "erf", "mrw", "mos", "iiq", "3fr", "dcr", "kdc", "mef", "rwl", "x3f",
];

/// 現代容器格式副檔名
pub const MODERN_EXTS: &[&str] = &["jxl", "avif", "avifs", "heic", "heif", "hif"];

/// 判斷是否為需要專用解碼器的格式（以檔頭為主、副檔名為輔）
pub fn detect(path: &Path, head: &[u8]) -> Option<ExtraFormat> {
    // JPEG XL：裸碼流簽章 FF 0A，或 ISOBMFF 容器簽章
    const JXL_CONTAINER: &[u8] = &[
        0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
    ];
    if head.starts_with(&[0xFF, 0x0A]) || head.starts_with(JXL_CONTAINER) {
        return Some(ExtraFormat::JpegXl);
    }

    // ISOBMFF：位元組 4..8 為 "ftyp"，緊接著是 major brand
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        match &head[8..12] {
            b"avif" | b"avis" => return Some(ExtraFormat::Avif),
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => {
                return Some(ExtraFormat::Heif)
            }
            _ => {}
        }
    }

    // RAW：依副檔名
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())?;
    if RAW_EXTS.contains(&ext.as_str()) {
        return Some(ExtraFormat::Raw);
    }
    None
}

/// 讀檔頭並判斷格式；非這些格式回傳 None（交回 image crate 處理）
pub fn sniff(path: &Path) -> Option<ExtraFormat> {
    let mut head = [0u8; 16];
    let n = File::open(path).ok()?.read(&mut head).ok()?;
    detect(path, &head[..n])
}

pub fn decode(path: &Path, fmt: ExtraFormat) -> Result<RgbaImage, String> {
    match fmt {
        ExtraFormat::JpegXl => decode_jxl(path),
        // AVIF 與 HEIC 同為 ISOBMFF 容器，heic crate 兩者皆可解
        ExtraFormat::Avif | ExtraFormat::Heif => decode_heif(path, fmt),
        ExtraFormat::Raw => decode_raw(path),
    }
}

fn decode_jxl(path: &Path) -> Result<RgbaImage, String> {
    let img = jxl_oxide::JxlImage::builder()
        .open(path)
        .map_err(|e| format!("JPEG XL 標頭解析失敗：{e}"))?;
    let render = img
        .render_frame(0)
        .map_err(|e| format!("JPEG XL 解碼失敗：{e}"))?;
    let fb = render.image_all_channels();
    let (w, h, ch) = (fb.width(), fb.height(), fb.channels());
    if w == 0 || h == 0 || ch == 0 {
        return Err("JPEG XL 影像尺寸無效".into());
    }
    let buf = fb.buf();
    if buf.len() < w * h * ch {
        return Err("JPEG XL 像素資料不足".into());
    }
    // jxl-oxide 輸出為 0.0–1.0 浮點；轉成 8-bit
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let mut out = RgbaImage::new(w as u32, h as u32);
    for (i, px) in out.pixels_mut().enumerate() {
        let s = i * ch;
        *px = match ch {
            1 => {
                let g = q(buf[s]);
                Rgba([g, g, g, 255])
            }
            2 => {
                let g = q(buf[s]);
                Rgba([g, g, g, q(buf[s + 1])])
            }
            3 => Rgba([q(buf[s]), q(buf[s + 1]), q(buf[s + 2]), 255]),
            _ => Rgba([q(buf[s]), q(buf[s + 1]), q(buf[s + 2]), q(buf[s + 3])]),
        };
    }
    Ok(out)
}

fn decode_heif(path: &Path, fmt: ExtraFormat) -> Result<RgbaImage, String> {
    let data = std::fs::read(path).map_err(|e| format!("讀取失敗：{e}"))?;
    let out = heic::DecoderConfig::new()
        .decode(&data, heic::PixelLayout::Rgba8)
        .map_err(|e| format!("{} 解碼失敗：{e}", fmt.name()))?;
    RgbaImage::from_raw(out.width, out.height, out.data)
        .ok_or_else(|| format!("{} 影像尺寸與像素資料不符", fmt.name()))
}

fn decode_raw(path: &Path) -> Result<RgbaImage, String> {
    // 0,0 = 不限制輸出尺寸（完整解析度）
    let srgb =
        imagepipe::simple_decode_8bit(path, 0, 0).map_err(|e| format!("RAW 解碼失敗：{e}"))?;
    let (w, h) = (srgb.width, srgb.height);
    if w == 0 || h == 0 || srgb.data.len() < w * h * 3 {
        return Err("RAW 影像資料無效".into());
    }
    // imagepipe 輸出 RGB（3 bytes/px），補上不透明 alpha
    let mut out = RgbaImage::new(w as u32, h as u32);
    for (i, px) in out.pixels_mut().enumerate() {
        let s = i * 3;
        *px = Rgba([srgb.data[s], srgb.data[s + 1], srgb.data[s + 2], 255]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    /// 組出 ISOBMFF 檔頭：size(4) + "ftyp" + brand
    fn ftyp(brand: &[u8; 4]) -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x00, 0x18];
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(brand);
        v.extend_from_slice(b"\0\0\0\0");
        v
    }

    #[test]
    fn detects_jxl_both_signatures() {
        // 裸碼流
        assert_eq!(
            detect(&p("a.jxl"), &[0xFF, 0x0A, 0x00, 0x00]),
            Some(ExtraFormat::JpegXl)
        );
        // ISOBMFF 容器
        let container = [
            0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
        ];
        assert_eq!(detect(&p("a.jxl"), &container), Some(ExtraFormat::JpegXl));
    }

    #[test]
    fn detects_avif_and_heic_brands() {
        assert_eq!(
            detect(&p("x.avif"), &ftyp(b"avif")),
            Some(ExtraFormat::Avif)
        );
        assert_eq!(
            detect(&p("x.avif"), &ftyp(b"avis")),
            Some(ExtraFormat::Avif)
        );
        for brand in [b"heic", b"heix", b"mif1", b"msf1"] {
            assert_eq!(
                detect(&p("IMG_0001.heic"), &ftyp(brand)),
                Some(ExtraFormat::Heif),
                "brand {:?}",
                std::str::from_utf8(brand).unwrap()
            );
        }
    }

    #[test]
    fn detects_raw_by_extension() {
        for ext in ["cr2", "NEF", "arw", "dng", "RW2"] {
            assert_eq!(
                detect(&p(&format!("shot.{ext}")), &[0x49, 0x49, 0x2A, 0x00]),
                Some(ExtraFormat::Raw),
                "ext {ext}"
            );
        }
    }

    #[test]
    fn ignores_formats_handled_by_image_crate() {
        // PNG / JPEG / GIF 檔頭不應被攔截
        assert_eq!(detect(&p("a.png"), &[0x89, b'P', b'N', b'G']), None);
        assert_eq!(detect(&p("a.jpg"), &[0xFF, 0xD8, 0xFF, 0xE0]), None);
        assert_eq!(detect(&p("a.gif"), b"GIF89a......"), None);
        // 一般 TIFF（非 RAW 副檔名）也不該被當成 RAW
        assert_eq!(detect(&p("a.tif"), &[0x49, 0x49, 0x2A, 0x00]), None);
        // MP4 是 ISOBMFF 但不是圖片
        assert_eq!(detect(&p("v.mp4"), &ftyp(b"isom")), None);
    }

    #[test]
    fn tolerates_short_head() {
        assert_eq!(detect(&p("x.png"), &[]), None);
        assert_eq!(detect(&p("x.png"), &[0xFF]), None);
        // 檔頭太短但副檔名是 RAW → 仍判定為 RAW
        assert_eq!(detect(&p("x.cr2"), &[0x49]), Some(ExtraFormat::Raw));
    }
}
