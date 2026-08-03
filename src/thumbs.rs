//! 膠捲條用的縮圖產生與快取。
//!
//! 效能策略（由快到慢，取第一個成功的）：
//! 1. **EXIF 內嵌縮圖**：JPEG 與 RAW 幾乎都內嵌一張，零解碼成本
//! 2. **RAW 內嵌預覽**：重用 `raw_preview`，數十毫秒
//! 3. **JPEG DCT 縮放解碼**：重用 `jpeg_fast`，比全解快數倍
//! 4. 一般解碼後縮小（其餘格式）
//!
//! 只為「可見的格子」產生縮圖，且用獨立的小型 LRU 快取，
//! 不與主圖的解碼快取搶預算。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::ColorImage;

/// 縮圖的長邊像素數。膠捲條高度約 96pt，在 2× DPI 下需要 192px，
/// 取 256 讓高 DPI 螢幕也清晰，同時單張只佔 256KB。
pub const THUMB_SIZE: u32 = 256;

/// 縮圖快取上限（張數）。256×256×4 ≈ 256KB，300 張約 77MB。
const THUMB_CACHE_CAP: usize = 300;

/// 產生一張縮圖（在背景執行緒呼叫）
pub fn make(path: &Path) -> Option<ColorImage> {
    let img = load_small(path)?;
    Some(fit_into(&img, THUMB_SIZE))
}

fn load_small(path: &Path) -> Option<image::RgbaImage> {
    // 1. EXIF 內嵌縮圖（最快）——順帶涵蓋多數 JPEG 與 RAW
    if let Some(t) = exif_thumbnail(path) {
        return Some(t);
    }
    // 2. RAW 內嵌預覽
    if crate::extra_formats::sniff(path) == Some(crate::extra_formats::ExtraFormat::Raw) {
        if let Some(p) = crate::raw_preview::extract(path) {
            return Some(p.image);
        }
    }
    // 3. JPEG 用 DCT 縮放解碼
    if let Some((w, h)) = crate::jpeg_fast::dimensions(path) {
        let scale = crate::jpeg_fast::pick_scale((w, h), THUMB_SIZE);
        if let Ok((img, _)) = crate::jpeg_fast::decode_scaled(path, scale) {
            return Some(img);
        }
    }
    // 4. 其餘格式走一般解碼
    crate::loader::decode_static(path).ok().map(|(img, _)| img)
}

/// 只讀檔頭前段找 EXIF（APP1）裡的內嵌縮圖。
/// 刻意不用 image crate 的 JpegDecoder——它建構時就 read_to_end，
/// 對 30MB 的 JPEG 會把整個檔案讀進記憶體，膠捲條一次數十張會拖垮 I/O。
fn exif_thumbnail(path: &Path) -> Option<image::RgbaImage> {
    use std::io::Read;
    const HEAD: usize = 256 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(HEAD);
    f.by_ref().take(HEAD as u64).read_to_end(&mut buf).ok()?;

    // JPEG：掃描 APP1 段找 "Exif\0\0"，其後是 TIFF 結構
    if buf.len() > 4 && buf[0] == 0xFF && buf[1] == 0xD8 {
        let tiff = find_exif_tiff(&buf)?;
        let spans = crate::raw_preview::scan_tiff_spans(&buf[tiff..]);
        // IFD1 的縮圖通常是最後也最小的那個，挑最大的仍最合理
        let best = spans.iter().max_by_key(|s| s.len)?;
        let start = tiff + best.offset;
        let end = (start + best.len).min(buf.len());
        return image::load_from_memory_with_format(&buf[start..end], image::ImageFormat::Jpeg)
            .ok()
            .map(|d| d.into_rgba8());
    }
    None
}

/// 在 JPEG 檔頭中找出 EXIF 的 TIFF 結構起點
fn find_exif_tiff(buf: &[u8]) -> Option<usize> {
    let mut p = 2usize;
    while p + 4 <= buf.len() {
        if buf[p] != 0xFF {
            p += 1;
            continue;
        }
        let marker = buf[p + 1];
        if marker == 0xD8 || (0xD0..=0xD9).contains(&marker) {
            p += 2;
            continue;
        }
        if marker == 0xDA {
            break; // 進入影像資料
        }
        let len = u16::from_be_bytes([*buf.get(p + 2)?, *buf.get(p + 3)?]) as usize;
        if marker == 0xE1 && buf.len() >= p + 10 && &buf[p + 4..p + 10] == b"Exif\0\0" {
            return Some(p + 10);
        }
        p += 2 + len;
    }
    None
}

/// 等比縮到長邊 `target`，逐次減半再做最後一次取樣（快且無鋸齒）
fn fit_into(src: &image::RgbaImage, target: u32) -> ColorImage {
    let mut img = std::borrow::Cow::Borrowed(src);
    while img.width() / 2 >= target && img.height() / 2 >= target {
        img = std::borrow::Cow::Owned(image::imageops::resize(
            img.as_ref(),
            img.width() / 2,
            img.height() / 2,
            image::imageops::FilterType::Triangle,
        ));
    }
    let (w, h) = (img.width().max(1), img.height().max(1));
    let scale = (target as f32 / w.max(h) as f32).min(1.0);
    let (nw, nh) = (
        ((w as f32 * scale) as u32).max(1),
        ((h as f32 * scale) as u32).max(1),
    );
    let out = image::imageops::resize(
        img.as_ref(),
        nw,
        nh,
        image::imageops::FilterType::CatmullRom,
    );
    ColorImage::from_rgba_unmultiplied([nw as usize, nh as usize], out.as_raw())
}

/// 縮圖的小型 LRU 快取（與主圖快取分開，互不搶預算）
#[derive(Default)]
pub struct ThumbCache {
    map: HashMap<PathBuf, (Arc<ColorImage>, u64)>,
    tick: u64,
}

impl ThumbCache {
    pub fn get(&mut self, path: &Path) -> Option<Arc<ColorImage>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(path).map(|e| {
            e.1 = t;
            e.0.clone()
        })
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.map.contains_key(path)
    }

    pub fn insert(&mut self, path: PathBuf, img: Arc<ColorImage>) {
        self.tick += 1;
        self.map.insert(path, (img, self.tick));
        while self.map.len() > THUMB_CACHE_CAP {
            let Some(victim) = self
                .map
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(p, _)| p.clone())
            else {
                break;
            };
            self.map.remove(&victim);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zoetrope-th-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fit_into_preserves_aspect_and_caps_size() {
        let src = image::RgbaImage::new(1600, 800);
        let t = fit_into(&src, 256);
        assert_eq!(t.size, [256, 128], "應等比縮到長邊 256");

        // 比目標小的圖不該被放大
        let small = image::RgbaImage::new(100, 50);
        let t = fit_into(&small, 256);
        assert_eq!(t.size, [100, 50]);
    }

    #[test]
    fn thumbnail_from_ordinary_png() {
        let d = tmp("png");
        let p = d.join("a.png");
        image::RgbImage::from_fn(800, 600, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        })
        .save(&p)
        .unwrap();
        let t = make(&p).expect("應產生縮圖");
        assert!(t.size[0] <= 256 && t.size[1] <= 256);
        assert_eq!(t.size, [256, 192]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn thumbnail_from_large_jpeg_uses_scaled_decode() {
        let d = tmp("jpg");
        let p = d.join("big.jpg");
        image::RgbImage::from_fn(3000, 2000, |x, y| {
            image::Rgb([(x / 12 % 256) as u8, (y / 8 % 256) as u8, 90])
        })
        .save(&p)
        .unwrap();
        let t = make(&p).expect("應產生縮圖");
        // 長邊固定為 256，短邊依原始長寬比（3:2）換算，容許 1px 捨去誤差
        assert_eq!(t.size[0], 256);
        let expect_h = (256.0 * 2000.0 / 3000.0) as usize;
        assert!(
            t.size[1].abs_diff(expect_h) <= 1,
            "高度應約 {expect_h}，實際 {}",
            t.size[1]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_evicts_least_recently_used() {
        let mut c = ThumbCache::default();
        let img = || Arc::new(ColorImage::new([4, 4], eframe::egui::Color32::RED));
        for i in 0..THUMB_CACHE_CAP + 5 {
            c.insert(PathBuf::from(format!("f{i}")), img());
        }
        assert_eq!(c.len(), THUMB_CACHE_CAP, "應維持在容量上限");
        // 最早插入的應已被淘汰
        assert!(!c.contains(Path::new("f0")));
        assert!(c.contains(Path::new(&format!("f{}", THUMB_CACHE_CAP + 4))));
    }

    #[test]
    fn cache_get_refreshes_recency() {
        let mut c = ThumbCache::default();
        let img = || Arc::new(ColorImage::new([2, 2], eframe::egui::Color32::BLUE));
        for i in 0..THUMB_CACHE_CAP {
            c.insert(PathBuf::from(format!("k{i}")), img());
        }
        // 讓 k0 變成最近使用
        assert!(c.get(Path::new("k0")).is_some());
        c.insert(PathBuf::from("new"), img());
        assert!(c.contains(Path::new("k0")), "剛存取過的不該被淘汰");
    }

    #[test]
    fn missing_or_broken_file_returns_none() {
        let d = tmp("bad");
        let p = d.join("broken.png");
        std::fs::write(&p, b"not a png").unwrap();
        assert!(make(&p).is_none());
        assert!(make(&d.join("nonexistent.png")).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
