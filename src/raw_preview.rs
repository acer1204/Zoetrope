//! 從相機 RAW 檔中抽取內嵌的 JPEG 預覽。
//!
//! 幾乎所有 RAW 都內嵌一張相機自己顯影好的 JPEG（多為全解析度）。
//! 直接解這張只要數十毫秒，而完整 demosaic 顯影要 1–2 秒——
//! 對「看圖」來說預覽就是使用者想看的畫面，因此優先使用。
//!
//! 支援兩種容器：
//! - **TIFF 系**（CR2 / NEF / ARW / DNG / ORF / PEF / RW2…）：走 IFD 鏈與 SubIFD
//! - **ISOBMFF 系**（CR3）：走 box 樹找 PRVW／THMB

use std::path::Path;

/// 檔案中一段 JPEG 資料的位置
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JpegSpan {
    pub offset: usize,
    pub len: usize,
}

/// 解析上限，避免惡意或損毀檔案造成無窮迴圈
const MAX_IFDS: usize = 32;
const MAX_SUBIFD_DEPTH: u32 = 3;
const MAX_BOX_DEPTH: u32 = 4;
/// 小於這個邊長的預覽視為縮圖，不足以當顯示用
const MIN_USEFUL_DIM: u32 = 512;

struct Tiff<'a> {
    d: &'a [u8],
    be: bool,
}

impl<'a> Tiff<'a> {
    fn u16(&self, o: usize) -> Option<u16> {
        let b = self.d.get(o..o + 2)?;
        Some(if self.be {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        })
    }
    fn u32(&self, o: usize) -> Option<u32> {
        let b = self.d.get(o..o + 4)?;
        Some(if self.be {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        })
    }
    /// 讀取 IFD 條目的值；只處理我們需要的 SHORT/LONG 純量與陣列首項
    fn entry_value(&self, entry: usize) -> Option<u32> {
        let ty = self.u16(entry + 2)?;
        let count = self.u32(entry + 4)?;
        let val_off = entry + 8;
        match ty {
            3 => {
                // SHORT：≤2 個直接內嵌，否則 val_off 是偏移
                if count <= 2 {
                    self.u16(val_off).map(u32::from)
                } else {
                    let off = self.u32(val_off)? as usize;
                    self.u16(off).map(u32::from)
                }
            }
            4 => {
                // LONG：1 個直接內嵌
                if count <= 1 {
                    self.u32(val_off)
                } else {
                    let off = self.u32(val_off)? as usize;
                    self.u32(off)
                }
            }
            _ => None,
        }
    }
    /// 取出 LONG/SHORT 陣列（用於 SubIFDs 與多 strip 的情形）
    fn entry_array(&self, entry: usize) -> Vec<u32> {
        let (Some(ty), Some(count)) = (self.u16(entry + 2), self.u32(entry + 4)) else {
            return Vec::new();
        };
        let count = count.min(64) as usize; // 防爆
        let elem = match ty {
            3 => 2usize,
            4 => 4usize,
            _ => return Vec::new(),
        };
        let inline = count * elem <= 4;
        let base = if inline {
            entry + 8
        } else {
            match self.u32(entry + 8) {
                Some(o) => o as usize,
                None => return Vec::new(),
            }
        };
        (0..count)
            .filter_map(|i| {
                let o = base + i * elem;
                if elem == 2 {
                    self.u16(o).map(u32::from)
                } else {
                    self.u32(o)
                }
            })
            .collect()
    }
}

/// 掃描 TIFF 系 RAW，回傳所有找得到的 JPEG 區段
fn scan_tiff(data: &[u8]) -> Vec<JpegSpan> {
    if data.len() < 8 {
        return Vec::new();
    }
    let be = match &data[0..2] {
        b"MM" => true,
        b"II" => false,
        _ => return Vec::new(),
    };
    let t = Tiff { d: data, be };
    // magic 通常是 42；Canon CR2 用 42、Olympus ORF 用 0x4F52/0x5352，一律放行
    let Some(first) = t.u32(4) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    let mut visited = 0usize;
    walk_ifd(&t, first as usize, 0, &mut found, &mut visited);
    found.retain(|s| is_jpeg(data, *s));
    found
}

fn walk_ifd(t: &Tiff, off: usize, depth: u32, out: &mut Vec<JpegSpan>, visited: &mut usize) {
    let mut off = off;
    while off != 0 && *visited < MAX_IFDS {
        *visited += 1;
        let Some(n) = t.u16(off) else { return };
        let n = n.min(512) as usize;
        let entries = off + 2;

        let mut jpeg_off = None; // 0x0201 JPEGInterchangeFormat
        let mut jpeg_len = None; // 0x0202 JPEGInterchangeFormatLength
        let mut strips: Vec<u32> = Vec::new(); // 0x0111 StripOffsets
        let mut strip_lens: Vec<u32> = Vec::new(); // 0x0117 StripByteCounts
        let mut compression = None; // 0x0103
        let mut subifds: Vec<u32> = Vec::new(); // 0x014A

        for i in 0..n {
            let e = entries + i * 12;
            let Some(tag) = t.u16(e) else { break };
            match tag {
                0x0201 => jpeg_off = t.entry_value(e),
                0x0202 => jpeg_len = t.entry_value(e),
                0x0111 => strips = t.entry_array(e),
                0x0117 => strip_lens = t.entry_array(e),
                0x0103 => compression = t.entry_value(e),
                0x014A => subifds = t.entry_array(e),
                _ => {}
            }
        }

        if let (Some(o), Some(l)) = (jpeg_off, jpeg_len) {
            push_span(t.d, o as usize, l as usize, out);
        }
        // Compression 6/7 = JPEG，此時 strip 內容就是 JPEG 資料
        if matches!(compression, Some(6) | Some(7)) && strips.len() == 1 && strip_lens.len() == 1 {
            push_span(t.d, strips[0] as usize, strip_lens[0] as usize, out);
        }

        if depth < MAX_SUBIFD_DEPTH {
            for s in subifds {
                walk_ifd(t, s as usize, depth + 1, out, visited);
            }
        }

        // 下一個 IFD
        let next_off = entries + n * 12;
        off = match t.u32(next_off) {
            Some(v) if (v as usize) < t.d.len() && v as usize != off => v as usize,
            _ => 0,
        };
    }
}

fn push_span(data: &[u8], offset: usize, len: usize, out: &mut Vec<JpegSpan>) {
    if len == 0 || offset >= data.len() || offset.saturating_add(len) > data.len() {
        return;
    }
    out.push(JpegSpan { offset, len });
}

fn is_jpeg(data: &[u8], s: JpegSpan) -> bool {
    data.get(s.offset..s.offset + 3)
        .map(|b| b == [0xFF, 0xD8, 0xFF])
        .unwrap_or(false)
}

/// 掃描 ISOBMFF 系（Canon CR3），預覽放在 uuid box 底下的 PRVW／THMB
fn scan_isobmff(data: &[u8]) -> Vec<JpegSpan> {
    let mut out = Vec::new();
    walk_boxes(data, 0, data.len(), 0, &mut out);
    out.retain(|s| is_jpeg(data, *s));
    out
}

fn be_u32(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn walk_boxes(data: &[u8], start: usize, end: usize, depth: u32, out: &mut Vec<JpegSpan>) {
    if depth > MAX_BOX_DEPTH {
        return;
    }
    let mut p = start;
    while p + 8 <= end {
        let Some(size32) = be_u32(data, p) else {
            return;
        };
        let kind = &data[p + 4..p + 8];

        // size 0 = 延伸到本層結尾；size 1 = 之後接 64-bit 長度（CR3 的 mdat 會用）
        let (body, size) = match size32 {
            0 => (p + 8, end - p),
            1 => {
                let Some(hi) = be_u32(data, p + 8) else {
                    return;
                };
                let Some(lo) = be_u32(data, p + 12) else {
                    return;
                };
                let s = ((hi as u64) << 32 | lo as u64) as usize;
                if s < 16 {
                    return;
                }
                (p + 16, s)
            }
            s if (s as usize) < 8 => return,
            s => (p + 8, s as usize),
        };
        let body_end = (p + size).min(end);
        if body > body_end {
            return;
        }

        match kind {
            b"PRVW" | b"THMB" => take_jpeg_from_box(data, body, body_end, out),
            // uuid payload = 16 bytes UUID + 可能存在的自訂標頭，之後才是子 box
            // （Canon 預覽容器就多了 8 bytes）。直接找 PRVW/THMB 標籤最穩健。
            b"uuid" => {
                if body + 16 <= body_end {
                    scan_for_preview_tags(data, body + 16, body_end, out);
                }
            }
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                walk_boxes(data, body, body_end, depth + 1, out);
            }
            _ => {}
        }

        p += size;
    }
}

/// PRVW / THMB 的 payload 前面有小段尺寸欄位，長度隨機型而異；
/// 直接找 JPEG SOI 比硬套欄位配置穩健。
fn take_jpeg_from_box(data: &[u8], body: usize, body_end: usize, out: &mut Vec<JpegSpan>) {
    if body >= body_end {
        return;
    }
    if let Some(rel) = find_soi(&data[body..body_end]) {
        push_span(data, body + rel, body_end - (body + rel), out);
    }
}

/// 在區間內尋找 PRVW／THMB box 標籤，並以標籤前 4 bytes 當作 box 長度解析。
/// 用於 payload 前有未知長度標頭、無法直接照 box 樹走的情形。
fn scan_for_preview_tags(data: &[u8], start: usize, end: usize, out: &mut Vec<JpegSpan>) {
    const SCAN_LIMIT: usize = 8 << 20; // 只掃前 8 MB，避免對超大 box 做無謂掃描
    let end = end.min(start.saturating_add(SCAN_LIMIT)).min(data.len());
    if start + 8 > end {
        return;
    }
    let mut i = start + 4; // 標籤前面要有 4 bytes 的 size 欄位
    while i + 4 <= end {
        let tag = &data[i..i + 4];
        if tag == b"PRVW" || tag == b"THMB" {
            let box_start = i - 4;
            if let Some(size) = be_u32(data, box_start) {
                let box_end = (box_start + size as usize).min(end);
                take_jpeg_from_box(data, box_start + 8, box_end, out);
            }
        }
        i += 1;
    }
}

fn find_soi(buf: &[u8]) -> Option<usize> {
    // 只在前面一小段找，避免掃到影像資料裡的巧合位元組
    let limit = buf.len().min(256);
    (0..limit.saturating_sub(2)).find(|&i| buf[i..i + 3] == [0xFF, 0xD8, 0xFF])
}

/// 從 JPEG 的 SOF 標記讀出尺寸（不解碼）
fn jpeg_dimensions(buf: &[u8]) -> Option<(u32, u32)> {
    let mut p = 2usize; // 跳過 SOI
    while p + 4 <= buf.len() {
        if buf[p] != 0xFF {
            p += 1;
            continue;
        }
        let marker = buf[p + 1];
        // 無酬載的標記
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 || marker == 0xFF {
            p += 2;
            continue;
        }
        let len = u16::from_be_bytes([buf[p + 2], buf[p + 3]]) as usize;
        // SOF0..SOF15（跳過 DHT=C4、JPG=C8、DAC=CC）
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([*buf.get(p + 5)?, *buf.get(p + 6)?]) as u32;
            let w = u16::from_be_bytes([*buf.get(p + 7)?, *buf.get(p + 8)?]) as u32;
            return Some((w, h));
        }
        if marker == 0xDA {
            break; // 進入掃描資料，之後沒有標頭了
        }
        p += 2 + len;
    }
    None
}

/// RAW 內嵌預覽的解碼結果
pub struct Preview {
    pub image: image::RgbaImage,
    /// 預覽的像素尺寸
    pub size: (u32, u32),
}

/// 從 RAW 檔抽出「最大的」內嵌 JPEG 預覽並解碼。
/// 找不到可用預覽時回傳 None，呼叫端應回退到完整顯影。
pub fn extract(path: &Path) -> Option<Preview> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 16 {
        return None;
    }

    let mut spans = scan_tiff(&data);
    if spans.is_empty() {
        // 非 TIFF 系：試 ISOBMFF（CR3）
        if data.len() >= 12 && &data[4..8] == b"ftyp" {
            spans = scan_isobmff(&data);
        }
    }
    if spans.is_empty() {
        return None;
    }

    // 以實際像素數挑最大的一張（位元組大小不一定等比）
    let mut best: Option<(u32, JpegSpan, (u32, u32))> = None;
    for s in spans {
        let buf = &data[s.offset..s.offset + s.len];
        let Some((w, h)) = jpeg_dimensions(buf) else {
            continue;
        };
        if w.max(h) < MIN_USEFUL_DIM {
            continue; // 只是縮圖，跳過
        }
        let px = w.saturating_mul(h);
        if best.as_ref().is_none_or(|(bp, _, _)| px > *bp) {
            best = Some((px, s, (w, h)));
        }
    }
    let (_, span, size) = best?;

    let buf = &data[span.offset..span.offset + span.len];
    let img = image::load_from_memory_with_format(buf, image::ImageFormat::Jpeg).ok()?;
    Some(Preview {
        image: img.into_rgba8(),
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 組一個最小的 TIFF：IFD0 帶 JPEGInterchangeFormat 指向一段假 JPEG
    fn tiny_tiff_with_jpeg(jpeg: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"II");
        d.extend_from_slice(&42u16.to_le_bytes());
        d.extend_from_slice(&8u32.to_le_bytes()); // IFD0 @ 8

        let entry_count = 2u16;
        let ifd_size = 2 + entry_count as usize * 12 + 4;
        let jpeg_off = (8 + ifd_size) as u32;

        d.extend_from_slice(&entry_count.to_le_bytes());
        // 0x0201 JPEGInterchangeFormat (LONG)
        d.extend_from_slice(&0x0201u16.to_le_bytes());
        d.extend_from_slice(&4u16.to_le_bytes());
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(&jpeg_off.to_le_bytes());
        // 0x0202 JPEGInterchangeFormatLength (LONG)
        d.extend_from_slice(&0x0202u16.to_le_bytes());
        d.extend_from_slice(&4u16.to_le_bytes());
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
        d.extend_from_slice(jpeg);
        d
    }

    /// 產生一張真的 JPEG，方便驗證尺寸解析
    fn real_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, _| image::Rgb([(x % 256) as u8, 128, 200]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Jpeg)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn parses_jpeg_dimensions_from_sof() {
        let j = real_jpeg(640, 480);
        assert_eq!(jpeg_dimensions(&j), Some((640, 480)));
    }

    #[test]
    fn finds_preview_in_tiff_ifd() {
        let j = real_jpeg(1024, 768);
        let tiff = tiny_tiff_with_jpeg(&j);
        let spans = scan_tiff(&tiff);
        assert_eq!(spans.len(), 1, "應找到一段 JPEG");
        let s = spans[0];
        assert_eq!(&tiff[s.offset..s.offset + 3], &[0xFF, 0xD8, 0xFF]);
        assert_eq!(
            jpeg_dimensions(&tiff[s.offset..s.offset + s.len]),
            Some((1024, 768))
        );
    }

    #[test]
    fn skips_thumbnail_sized_previews() {
        // 128×96 低於 MIN_USEFUL_DIM，extract 應視為不可用
        let dir = std::env::temp_dir().join(format!("zoetrope-prev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tiny.cr2");
        std::fs::write(&p, tiny_tiff_with_jpeg(&real_jpeg(128, 96))).unwrap();
        assert!(extract(&p).is_none(), "縮圖尺寸的預覽不該被採用");

        // 換成 2000×1500 就應該可用
        let p2 = dir.join("big.cr2");
        std::fs::write(&p2, tiny_tiff_with_jpeg(&real_jpeg(2000, 1500))).unwrap();
        let prev = extract(&p2).expect("應抽出預覽");
        assert_eq!(prev.size, (2000, 1500));
        assert_eq!((prev.image.width(), prev.image.height()), (2000, 1500));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_non_raw_input() {
        let dir = std::env::temp_dir().join(format!("zoetrope-prev2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("plain.cr2");
        std::fs::write(&p, real_jpeg(800, 600)).unwrap(); // 純 JPEG，不是 TIFF 容器
        assert!(extract(&p).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handles_truncated_and_garbage() {
        assert!(scan_tiff(&[]).is_empty());
        assert!(scan_tiff(b"II").is_empty());
        assert!(scan_tiff(b"XX\x2a\x00\x08\x00\x00\x00").is_empty());
        // 指向檔外的偏移不應 panic
        let mut bad = tiny_tiff_with_jpeg(&real_jpeg(1024, 768));
        bad.truncate(40);
        let _ = scan_tiff(&bad);
    }

    #[test]
    fn finds_preview_in_cr3_prvw_box() {
        let j = real_jpeg(1600, 1067);
        let mut d = Vec::new();
        // ftyp box
        let ftyp_size = 8u32 + 8;
        d.extend_from_slice(&ftyp_size.to_be_bytes());
        d.extend_from_slice(b"ftyp");
        d.extend_from_slice(b"crx ");
        d.extend_from_slice(&0u32.to_be_bytes());
        // uuid box：16 bytes UUID + 內含 PRVW
        let prvw_hdr = 12usize; // 模擬自訂標頭
        let prvw_size = 8 + prvw_hdr + j.len();
        let uuid_size = 8 + 16 + prvw_size;
        d.extend_from_slice(&(uuid_size as u32).to_be_bytes());
        d.extend_from_slice(b"uuid");
        d.extend_from_slice(&[0xEA; 16]);
        d.extend_from_slice(&(prvw_size as u32).to_be_bytes());
        d.extend_from_slice(b"PRVW");
        d.extend_from_slice(&[0u8; 12]);
        d.extend_from_slice(&j);

        let spans = scan_isobmff(&d);
        assert_eq!(spans.len(), 1, "應在 PRVW box 找到 JPEG");
        assert_eq!(
            jpeg_dimensions(&d[spans[0].offset..spans[0].offset + spans[0].len]),
            Some((1600, 1067))
        );
    }

    /// 真實 CR3 的 uuid payload 在 16 bytes UUID 後還有 8 bytes 標頭才接 PRVW，
    /// 且 mdat 使用 64-bit size——兩者都必須正確處理
    #[test]
    fn handles_cr3_uuid_header_and_64bit_mdat() {
        let j = real_jpeg(1620, 1080);
        let mut d = Vec::new();
        // ftyp
        d.extend_from_slice(&24u32.to_be_bytes());
        d.extend_from_slice(b"ftyp");
        d.extend_from_slice(b"crx ");
        d.extend_from_slice(&[0u8; 12]);
        // uuid：16 bytes UUID + 8 bytes 額外標頭 + PRVW box
        let prvw_hdr = 16usize; // PRVW 自身的尺寸欄位
        let prvw_size = 8 + prvw_hdr + j.len();
        let uuid_size = 8 + 16 + 8 + prvw_size;
        d.extend_from_slice(&(uuid_size as u32).to_be_bytes());
        d.extend_from_slice(b"uuid");
        d.extend_from_slice(&[
            0xEA, 0xF4, 0x2B, 0x5E, 0x1C, 0x98, 0x4B, 0x88, 0xB9, 0xFB, 0xB7, 0xDC, 0x40, 0x6E,
            0x4D, 0x16,
        ]);
        d.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]); // 額外標頭
        d.extend_from_slice(&(prvw_size as u32).to_be_bytes());
        d.extend_from_slice(b"PRVW");
        d.extend_from_slice(&[0u8; 16]);
        d.extend_from_slice(&j);
        // mdat 以 64-bit size 收尾
        d.extend_from_slice(&1u32.to_be_bytes());
        d.extend_from_slice(b"mdat");
        d.extend_from_slice(&(24u64).to_be_bytes());
        d.extend_from_slice(&[0u8; 8]);

        let spans = scan_isobmff(&d);
        assert_eq!(spans.len(), 1, "應找到 PRVW 內的 JPEG");
        assert_eq!(
            jpeg_dimensions(&d[spans[0].offset..spans[0].offset + spans[0].len]),
            Some((1620, 1080))
        );
    }
}
