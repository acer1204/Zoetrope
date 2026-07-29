use std::fs::File;
use std::io::BufReader;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui::{ColorImage, Context};
use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::JpegDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::metadata::Orientation;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageReader, RgbaImage};

use crate::cache::Cache;
use crate::dirlist;
use crate::types::*;

/// 背景解碼引擎：
/// - 高/低優先權雙佇列（目前圖片 vs. 鄰居預載）
/// - 動畫串流：第一格解出立即回報，之後邊解邊送
/// - generation 遞增作廢機制：快速翻頁時中斷過時的動畫解碼
pub struct Loader {
    hi_tx: Sender<Job>,
    lo_tx: Sender<Job>,
    pub events: Receiver<LoadEvent>,
    pub latest_gen: Arc<AtomicU64>,
    cache: Arc<Mutex<Cache>>,
}

impl Loader {
    pub fn new(ctx: Context, cache: Arc<Mutex<Cache>>) -> Self {
        let (hi_tx, hi_rx) = unbounded::<Job>();
        let (lo_tx, lo_rx) = unbounded::<Job>();
        let (ev_tx, ev_rx) = unbounded::<LoadEvent>();
        let latest_gen = Arc::new(AtomicU64::new(0));

        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .saturating_sub(1)
            .clamp(2, 4);

        for wi in 0..workers {
            let hi_rx = hi_rx.clone();
            let lo_rx = lo_rx.clone();
            let ev_tx = ev_tx.clone();
            let ctx = ctx.clone();
            let latest_gen = latest_gen.clone();
            let cache = cache.clone();
            std::thread::Builder::new()
                .name(format!("decode-{wi}"))
                .spawn(move || worker_loop(hi_rx, lo_rx, ev_tx, ctx, latest_gen, cache))
                .expect("spawn decode worker");
        }

        Self {
            hi_tx,
            lo_tx,
            events: ev_rx,
            latest_gen,
            cache,
        }
    }

    /// 載入目前要顯示的圖（高優先權）。回傳快取命中的完整結果（若有）。
    pub fn request_current(&self, path: PathBuf, generation: u64) -> Option<Arc<Decoded>> {
        self.latest_gen.store(generation, AtomicOrdering::SeqCst);
        let cached = self.cache.lock().unwrap().get(&path);
        match cached {
            Some(d) if d.complete => Some(d),
            partial => {
                let _ = self.hi_tx.send(Job::Load { path, generation });
                partial // 動畫可能只有預載的第一格：先顯示，完整解碼隨後送到
            }
        }
    }

    /// 只查快取，不觸發任何解碼工作
    pub fn peek(&self, path: &Path) -> Option<Arc<Decoded>> {
        self.cache.lock().unwrap().get(path)
    }

    pub fn request_prefetch(&self, path: PathBuf) {
        if !self.cache.lock().unwrap().contains(&path) {
            let _ = self.lo_tx.send(Job::Prefetch { path });
        }
    }

    pub fn request_scan(&self, dir: PathBuf, generation: u64) {
        let _ = self.hi_tx.send(Job::ScanDir { dir, generation });
    }

    pub fn set_protected(&self, paths: Vec<PathBuf>) {
        self.cache.lock().unwrap().set_protected(paths);
    }

    pub fn evict(&self, path: &Path) {
        self.cache.lock().unwrap().remove(path);
    }

    pub fn cache_stats(&self) -> (usize, usize) {
        self.cache.lock().unwrap().stats()
    }
}

fn worker_loop(
    hi_rx: Receiver<Job>,
    lo_rx: Receiver<Job>,
    ev_tx: Sender<LoadEvent>,
    ctx: Context,
    latest_gen: Arc<AtomicU64>,
    cache: Arc<Mutex<Cache>>,
) {
    loop {
        // 先清空高優先權佇列，兩邊都空才阻塞等待
        let job = match hi_rx.try_recv() {
            Ok(j) => j,
            Err(_) => crossbeam_channel::select! {
                recv(hi_rx) -> j => match j { Ok(j) => j, Err(_) => return },
                recv(lo_rx) -> j => match j { Ok(j) => j, Err(_) => return },
            },
        };
        let send = |ev: LoadEvent| {
            let _ = ev_tx.send(ev);
            ctx.request_repaint();
        };
        match job {
            Job::ScanDir { dir, generation } => {
                let entries = dirlist::scan_dir(&dir);
                send(LoadEvent::DirListing {
                    generation,
                    entries,
                });
            }
            Job::Load { path, generation } => {
                let r = catch_unwind(AssertUnwindSafe(|| {
                    decode_streaming(&path, generation, &latest_gen, &send)
                }));
                match r {
                    Ok(Ok(decoded)) => {
                        let complete = decoded.complete;
                        let truncated = decoded.truncated;
                        cache.lock().unwrap().insert(path, Arc::new(decoded));
                        send(LoadEvent::Done {
                            generation,
                            complete,
                            truncated,
                        });
                    }
                    Ok(Err(msg)) => send(LoadEvent::Error {
                        generation,
                        message: msg,
                    }),
                    Err(_) => send(LoadEvent::Error {
                        generation,
                        message: "解碼器內部錯誤（已略過此檔）".into(),
                    }),
                }
            }
            Job::Prefetch { path } => {
                if cache.lock().unwrap().contains(&path) {
                    continue;
                }
                let r = catch_unwind(AssertUnwindSafe(|| decode_prefetch(&path)));
                if let Ok(Ok(decoded)) = r {
                    cache
                        .lock()
                        .unwrap()
                        .insert(path.clone(), Arc::new(decoded));
                    send(LoadEvent::Prefetched { path });
                }
                // 預載失敗不回報：使用者真的翻到那張時會以高優先權重試並顯示錯誤
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 解碼核心
// ---------------------------------------------------------------------------

fn ext_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// 以檔案內容（魔術位元組）判斷實際格式。
/// 副檔名亂標很常見（如 Instagram 下載的 JPEG 存成 .png），一律以內容為準。
fn sniff_animation_kind(path: &Path, ext: &str) -> String {
    let sniffed = ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.format());
    match sniffed {
        Some(image::ImageFormat::Gif) => "gif".into(),
        Some(image::ImageFormat::WebP) => "webp".into(),
        Some(image::ImageFormat::Png) => "png".into(),
        Some(_) => String::new(), // 確定是其他格式：跳過動畫探測，走靜態路徑
        None => ext.to_owned(),   // 嗅探不出來（罕見）：退回副檔名判斷
    }
}

fn err_str<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn open_buffered(path: &Path) -> Result<BufReader<File>, String> {
    File::open(path)
        .map(|f| BufReader::with_capacity(1 << 20, f))
        .map_err(|e| format!("無法開啟檔案：{e}"))
}

/// 目前圖片的完整解碼；邊解邊透過 `send` 送事件給 UI。
fn decode_streaming(
    path: &Path,
    generation: u64,
    latest_gen: &AtomicU64,
    send: &dyn Fn(LoadEvent),
) -> Result<Decoded, String> {
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let ext = ext_lower(path);

    // 檔頭尺寸先送（大圖解碼要幾百毫秒，先讓 UI 顯示尺寸與載入狀態）
    if let Ok((w, h)) = image::image_dimensions(path) {
        send(LoadEvent::Meta {
            generation,
            meta: ImageMeta {
                path: path.to_path_buf(),
                orig_size: [w, h],
                file_size,
                format: ext.to_uppercase(),
                animated: false,
                has_alpha: false,
            },
        });
    }

    let kind = sniff_animation_kind(path, &ext);
    if let Some((frames_iter, dims, fmt)) = open_animation(path, &kind)? {
        decode_animation(
            path,
            file_size,
            dims,
            fmt,
            frames_iter,
            true,
            generation,
            latest_gen,
            send,
        )
    } else {
        let (rgba, fmt) = decode_static(path)?;
        let orig_size = [rgba.width(), rgba.height()];
        let has_alpha = rgba_has_alpha(&rgba);
        let meta = ImageMeta {
            path: path.to_path_buf(),
            orig_size,
            file_size,
            format: fmt,
            animated: false,
            has_alpha,
        };
        send(LoadEvent::Meta {
            generation,
            meta: meta.clone(),
        });

        let base = Arc::new(to_color_image_clamped(rgba));
        let frame = FrameData {
            image: base.clone(),
            delay: Duration::ZERO,
        };
        send(LoadEvent::Frame {
            generation,
            index: 0,
            frame: frame.clone(),
        });

        let mips = build_mips(base);
        send(LoadEvent::Mips {
            generation,
            mips: mips.clone(),
        });

        let bytes = Decoded::compute_bytes(std::slice::from_ref(&frame), &mips);
        Ok(Decoded {
            meta,
            frames: vec![frame],
            mips,
            complete: true,
            truncated: false,
            bytes,
        })
    }
}

/// 預載：靜態圖全解（含 mip 鏈）；動畫只解第一格，翻到時再全解。
fn decode_prefetch(path: &Path) -> Result<Decoded, String> {
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let ext = ext_lower(path);
    let ignore = |_: LoadEvent| {};
    let kind = sniff_animation_kind(path, &ext);
    if let Some((frames_iter, dims, fmt)) = open_animation(path, &kind)? {
        let never = AtomicU64::new(u64::MAX); // 不會被作廢
        decode_animation(
            path,
            file_size,
            dims,
            fmt,
            frames_iter,
            false,
            u64::MAX,
            &never,
            &ignore,
        )
    } else {
        let (rgba, fmt) = decode_static(path)?;
        let orig_size = [rgba.width(), rgba.height()];
        let has_alpha = rgba_has_alpha(&rgba);
        let base = Arc::new(to_color_image_clamped(rgba));
        let frame = FrameData {
            image: base.clone(),
            delay: Duration::ZERO,
        };
        let mips = build_mips(base);
        let bytes = Decoded::compute_bytes(std::slice::from_ref(&frame), &mips);
        Ok(Decoded {
            meta: ImageMeta {
                path: path.to_path_buf(),
                orig_size,
                file_size,
                format: fmt,
                animated: false,
                has_alpha,
            },
            frames: vec![frame],
            mips,
            complete: true,
            truncated: false,
            bytes,
        })
    }
}

/// 嘗試以動畫格式開啟；非動畫（或解碼器開不起來）回傳 None 走靜態路徑，
/// 讓靜態路徑的內容偵測處理副檔名與內容不符的檔案。
#[allow(clippy::type_complexity)]
pub fn open_animation(
    path: &Path,
    kind: &str,
) -> Result<Option<(image::Frames<'static>, (u32, u32), String)>, String> {
    match kind {
        "gif" => {
            let Ok(dec) = GifDecoder::new(open_buffered(path)?) else {
                return Ok(None);
            };
            let dims = dec.dimensions();
            Ok(Some((dec.into_frames(), dims, "GIF".into())))
        }
        "webp" => {
            let Ok(dec) = WebPDecoder::new(open_buffered(path)?) else {
                return Ok(None);
            };
            if dec.has_animation() {
                let dims = dec.dimensions();
                Ok(Some((dec.into_frames(), dims, "WebP 動畫".into())))
            } else {
                Ok(None)
            }
        }
        "png" | "apng" => {
            let Ok(dec) = PngDecoder::new(open_buffered(path)?) else {
                return Ok(None);
            };
            if dec.is_apng().unwrap_or(false) {
                let dims = dec.dimensions();
                let Ok(apng) = dec.apng() else {
                    return Ok(None);
                };
                Ok(Some((apng.into_frames(), dims, "APNG".into())))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

/// 動畫串流解碼。`full` = false 時只解第一格（預載模式）。
#[allow(clippy::too_many_arguments)]
fn decode_animation(
    path: &Path,
    file_size: u64,
    dims: (u32, u32),
    fmt: String,
    frames_iter: image::Frames<'static>,
    full: bool,
    generation: u64,
    latest_gen: &AtomicU64,
    send: &dyn Fn(LoadEvent),
) -> Result<Decoded, String> {
    let mut meta = ImageMeta {
        path: path.to_path_buf(),
        orig_size: [dims.0, dims.1],
        file_size,
        format: fmt,
        animated: true,
        has_alpha: false,
    };

    let mut frames: Vec<FrameData> = Vec::new();
    let mut total_bytes = 0usize;
    let mut complete = true;
    let mut truncated = false;

    for (i, fr) in frames_iter.enumerate() {
        // 使用者已翻到別張：中止，快取保留已解出的部分
        if latest_gen.load(AtomicOrdering::Relaxed) != generation {
            complete = false;
            break;
        }
        let fr = match fr {
            Ok(f) => f,
            Err(e) => {
                if frames.is_empty() {
                    return Err(err_str(e));
                }
                truncated = true; // 檔尾損毀：保留已解出的影格
                break;
            }
        };
        let delay = normalize_delay(Duration::from(fr.delay()));
        let buf: RgbaImage = fr.into_buffer();
        if i == 0 {
            meta.has_alpha = rgba_has_alpha(&buf);
            send(LoadEvent::Meta {
                generation,
                meta: meta.clone(),
            });
        }
        let ci = Arc::new(to_color_image_clamped(buf));
        let fd = FrameData { image: ci, delay };
        total_bytes += fd.bytes();
        send(LoadEvent::Frame {
            generation,
            index: frames.len(),
            frame: fd.clone(),
        });
        frames.push(fd);

        if !full {
            complete = false; // 預載模式：只解第一格
            break;
        }
        if total_bytes > ANIM_BUDGET_BYTES {
            truncated = true;
            break;
        }
    }

    if frames.is_empty() {
        return Err("動畫沒有任何影格".into());
    }

    // 單格「動畫」當靜態圖處理，補 mip 鏈
    let mips = if complete && frames.len() == 1 {
        meta.animated = false;
        build_mips(frames[0].image.clone())
    } else {
        Vec::new()
    };
    if !mips.is_empty() {
        send(LoadEvent::Mips {
            generation,
            mips: mips.clone(),
        });
    }

    let bytes = Decoded::compute_bytes(&frames, &mips);
    Ok(Decoded {
        meta,
        frames,
        mips,
        complete,
        truncated,
        bytes,
    })
}

/// 靜態圖解碼（處理 JPEG EXIF 方向）
pub fn decode_static(path: &Path) -> Result<(RgbaImage, String), String> {
    // image crate 不支援的格式先走專用解碼器（JPEG XL / AVIF / HEIC / RAW）
    if let Some(fmt) = crate::extra_formats::sniff(path) {
        return crate::extra_formats::decode(path, fmt).map(|img| (img, fmt.name().to_owned()));
    }

    let reader = ImageReader::open(path)
        .map_err(|e| format!("無法開啟檔案：{e}"))?
        .with_guessed_format()
        .map_err(err_str)?;
    let format = reader.format();
    let fmt_name = format
        .map(|f| format!("{f:?}").to_uppercase())
        .unwrap_or_else(|| ext_lower(path).to_uppercase());

    let img = match format {
        Some(image::ImageFormat::Jpeg) => {
            let mut dec = JpegDecoder::new(open_buffered(path)?).map_err(err_str)?;
            let orientation = dec.orientation().unwrap_or(Orientation::NoTransforms);
            let mut di = DynamicImage::from_decoder(dec).map_err(err_str)?;
            di.apply_orientation(orientation);
            di
        }
        _ => reader.decode().map_err(err_str)?,
    };
    Ok((img.into_rgba8(), fmt_name))
}

fn normalize_delay(d: Duration) -> Duration {
    // 依瀏覽器慣例：0/超短延遲視為 100ms（大量 GIF 標 0 期望 10fps）
    if d < Duration::from_millis(20) {
        Duration::from_millis(100)
    } else {
        d
    }
}

fn rgba_has_alpha(img: &RgbaImage) -> bool {
    img.as_raw().chunks_exact(4).any(|p| p[3] != 255)
}

/// RgbaImage → egui ColorImage；超過 GPU 貼圖上限就逐次減半。
pub fn to_color_image_clamped(mut rgba: RgbaImage) -> ColorImage {
    while rgba.width() > MAX_TEX_DIM || rgba.height() > MAX_TEX_DIM {
        rgba = half_rgba(&rgba);
    }
    let size = [rgba.width() as usize, rgba.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

/// 2×2 箱形濾波減半（RGBA u8）
fn half_rgba(src: &RgbaImage) -> RgbaImage {
    let (w, h) = (src.width(), src.height());
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let sraw = src.as_raw();
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        let y0 = (y * 2).min(h - 1) as usize;
        let y1 = (y * 2 + 1).min(h - 1) as usize;
        for x in 0..nw {
            let x0 = (x * 2).min(w - 1) as usize;
            let x1 = (x * 2 + 1).min(w - 1) as usize;
            let idx = |xx: usize, yy: usize| (yy * w as usize + xx) * 4;
            let (a, b, c, d) = (idx(x0, y0), idx(x1, y0), idx(x0, y1), idx(x1, y1));
            let o = ((y * nw + x) * 4) as usize;
            for ch in 0..4 {
                let sum = sraw[a + ch] as u32
                    + sraw[b + ch] as u32
                    + sraw[c + ch] as u32
                    + sraw[d + ch] as u32;
                out[o + ch] = (sum / 4) as u8;
            }
        }
    }
    RgbaImage::from_raw(nw, nh, out).expect("half_rgba buffer size")
}

/// 建立 mip 鏈：[0]=基底，之後每層減半，縮到 MIP_MIN_DIM 以下為止。
/// 縮小檢視時取「不小於顯示尺寸」的最近層，消除線性取樣縮圖的鋸齒與閃爍。
pub fn build_mips(base: Arc<ColorImage>) -> Vec<Arc<ColorImage>> {
    let mut mips = vec![base];
    loop {
        let last = mips.last().unwrap();
        if last.size[0].max(last.size[1]) <= MIP_MIN_DIM {
            break;
        }
        mips.push(Arc::new(half_color_image(last)));
    }
    mips
}

/// 2×2 箱形濾波減半（egui Color32，預乘 alpha 下逐通道平均即正確）
pub fn half_color_image(src: &ColorImage) -> ColorImage {
    let [w, h] = src.size;
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = ColorImage::new([nw, nh], eframe::egui::Color32::TRANSPARENT);
    for y in 0..nh {
        let y0 = (y * 2).min(h - 1);
        let y1 = (y * 2 + 1).min(h - 1);
        for x in 0..nw {
            let x0 = (x * 2).min(w - 1);
            let x1 = (x * 2 + 1).min(w - 1);
            let ps = [
                src.pixels[y0 * w + x0],
                src.pixels[y0 * w + x1],
                src.pixels[y1 * w + x0],
                src.pixels[y1 * w + x1],
            ];
            let avg = |f: fn(&eframe::egui::Color32) -> u8| {
                (ps.iter().map(|p| f(p) as u32).sum::<u32>() / 4) as u8
            };
            out.pixels[y * nw + x] = eframe::egui::Color32::from_rgba_premultiplied(
                avg(|p| p.r()),
                avg(|p| p.g()),
                avg(|p| p.b()),
                avg(|p| p.a()),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_normalization() {
        assert_eq!(
            normalize_delay(Duration::from_millis(0)),
            Duration::from_millis(100)
        );
        assert_eq!(
            normalize_delay(Duration::from_millis(10)),
            Duration::from_millis(100)
        );
        assert_eq!(
            normalize_delay(Duration::from_millis(40)),
            Duration::from_millis(40)
        );
    }

    #[test]
    fn mip_chain_dims() {
        let base = Arc::new(ColorImage::new([4000, 3000], eframe::egui::Color32::WHITE));
        let mips = build_mips(base);
        assert_eq!(mips[0].size, [4000, 3000]);
        assert_eq!(mips[1].size, [2000, 1500]);
        assert_eq!(mips[2].size, [1000, 750]);
        assert!(mips.last().unwrap().size[0].max(mips.last().unwrap().size[1]) <= MIP_MIN_DIM);
    }

    #[test]
    fn half_odd_dims() {
        let img = ColorImage::new([5, 3], eframe::egui::Color32::WHITE);
        let h = half_color_image(&img);
        assert_eq!(h.size, [2, 1]);
        let img = ColorImage::new([1, 1], eframe::egui::Color32::WHITE);
        let h = half_color_image(&img);
        assert_eq!(h.size, [1, 1]);
    }
}
