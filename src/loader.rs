use std::fs::File;
use std::io::BufReader;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering as AtomicOrdering};
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

    // RAW 走漸進式：先送內嵌預覽讓畫面立刻出現，必要時再補完整顯影
    if crate::extra_formats::sniff(path) == Some(crate::extra_formats::ExtraFormat::Raw) {
        return decode_raw_progressive(path, file_size, generation, latest_gen, send);
    }

    // HDR / EXR：保留浮點資料，套色調映射後才顯示
    if is_hdr_file(path) {
        return decode_hdr(path, file_size, generation, send);
    }

    let kind = sniff_animation_kind(path, &ext);
    // 大 JPEG 走漸進式：先以 DCT 縮放解出低解析度版本，再補全解析度
    if kind.is_empty() && is_jpeg_file(path) {
        if let Some(d) = decode_jpeg_progressive(path, file_size, generation, latest_gen, send) {
            return d;
        }
    }

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
            hdr: None,
            meta,
            frames: vec![frame],
            mips,
            complete: true,
            truncated: false,
            bytes,
        })
    }
}

/// 漸進式第一階段要達到的最長邊。
/// 實測 6000×4000 的 JPEG：全解 223ms、1/2 101ms、1/4 58ms、1/8 46ms——
/// 1/4 之後收益遞減（Huffman 解碼那段省不掉），因此取能落在 1/4 的門檻。
const JPEG_FAST_TARGET: u32 = 1200;

/// 小於這個像素數的 JPEG 全解本來就夠快，多跑一次縮放解碼反而是浪費
const JPEG_PROGRESSIVE_MIN_PIXELS: u64 = 6_000_000;

fn is_jpeg_file(path: &Path) -> bool {
    ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.format())
        == Some(image::ImageFormat::Jpeg)
}

/// 送出一張影像的完整事件組（Meta → Frame → Mips）。
/// `orig_size` 是「這張圖在原始檔案中的像素尺寸」——漸進式的低解析度階段
/// 仍要回報全解析度，縮放比例顯示與 mip 選層才會正確、畫面也不會跳動。
fn emit_static(
    path: &Path,
    rgba: RgbaImage,
    orig_size: [u32; 2],
    file_size: u64,
    format: &str,
    generation: u64,
    send: &dyn Fn(LoadEvent),
) -> (ImageMeta, FrameData, Vec<Arc<ColorImage>>) {
    let has_alpha = rgba_has_alpha(&rgba);
    let meta = ImageMeta {
        path: path.to_path_buf(),
        orig_size,
        file_size,
        format: format.to_owned(),
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
    (meta, frame, mips)
}

/// 大 JPEG 的漸進式解碼：
/// 1. 以 DCT 係數縮放解出約 `JPEG_FAST_TARGET` 大小的版本 → 立即顯示
/// 2. 再解全解析度 → 取代同一格
///
/// 回傳 None 代表這張不適合漸進式（尺寸不夠大或標頭讀不到），
/// 呼叫端應走一般路徑。
fn decode_jpeg_progressive(
    path: &Path,
    file_size: u64,
    generation: u64,
    latest_gen: &AtomicU64,
    send: &dyn Fn(LoadEvent),
) -> Option<Result<Decoded, String>> {
    let (fw, fh) = crate::jpeg_fast::dimensions(path)?;
    if u64::from(fw) * u64::from(fh) < JPEG_PROGRESSIVE_MIN_PIXELS {
        return None;
    }
    let scale = crate::jpeg_fast::pick_scale((fw, fh), JPEG_FAST_TARGET);
    if scale <= 1 {
        return None; // 不夠大，直接全解比較划算
    }

    // EXIF 方向要與最終版本一致，否則替換時畫面會翻轉
    let orientation = jpeg_orientation(path);
    // 方向若含 90/270 度旋轉，回報的原始尺寸要跟著對調
    let swapped = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    let orig_size = if swapped { [fh, fw] } else { [fw, fh] };

    if let Ok((img, _)) = crate::jpeg_fast::decode_scaled(path, scale) {
        // 使用者已翻到別張就別再送了
        if latest_gen.load(AtomicOrdering::Relaxed) != generation {
            return None;
        }
        let mut di = DynamicImage::ImageRgba8(img);
        di.apply_orientation(orientation);
        emit_static(
            path,
            di.into_rgba8(),
            orig_size,
            file_size,
            "JPEG",
            generation,
            send,
        );
    }

    // 第二階段：全解析度
    if latest_gen.load(AtomicOrdering::Relaxed) != generation {
        // 已被作廢；回傳目前為止的低解析度結果讓快取留著也無妨，
        // 但為了不讓快取存到半成品，這裡回報未完成
        return None;
    }
    let full = match decode_static(path) {
        Ok((rgba, _)) => rgba,
        Err(e) => return Some(Err(e)),
    };
    let (meta, frame, mips) =
        emit_static(path, full, orig_size, file_size, "JPEG", generation, send);
    let bytes = Decoded::compute_bytes(std::slice::from_ref(&frame), &mips);
    Some(Ok(Decoded {
        hdr: None,
        meta,
        frames: vec![frame],
        mips,
        complete: true,
        truncated: false,
        bytes,
    }))
}

fn is_hdr_file(path: &Path) -> bool {
    matches!(
        ImageReader::open(path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.format()),
        Some(image::ImageFormat::OpenExr) | Some(image::ImageFormat::Hdr)
    )
}

/// HDR / EXR 解碼：保留 f16 浮點原始資料，並以預設曝光做色調映射後顯示。
///
/// 之前的做法是直接 `into_rgba8()`——那只是把線性值截斷成 0–255、
/// 完全沒做 gamma，而 egui 又把結果當 sRGB 解讀，導致整張明顯偏暗，
/// 同時亮部細節全部糊成一片白。
fn decode_hdr(
    path: &Path,
    file_size: u64,
    generation: u64,
    send: &dyn Fn(LoadEvent),
) -> Result<Decoded, String> {
    let reader = ImageReader::open(path)
        .map_err(|e| format!("無法開啟檔案：{e}"))?
        .with_guessed_format()
        .map_err(err_str)?;
    let fmt = match reader.format() {
        Some(image::ImageFormat::OpenExr) => "OpenEXR",
        _ => "Radiance HDR",
    };
    let img = reader.decode().map_err(err_str)?;
    let mut hdr = crate::hdr::from_dynamic(&img).ok_or("這個檔案沒有浮點像素資料")?;

    // 超過貼圖上限時在浮點域縮小（先壓亮度再縮會讓高光邊緣出現暗環）
    let max = max_tex_side() as usize;
    while hdr.size[0] > max || hdr.size[1] > max {
        hdr = hdr.halved();
    }

    let orig_size = [hdr.size[0] as u32, hdr.size[1] as u32];
    let meta = ImageMeta {
        path: path.to_path_buf(),
        orig_size,
        file_size,
        format: fmt.to_owned(),
        animated: false,
        has_alpha: true,
    };
    send(LoadEvent::Meta {
        generation,
        meta: meta.clone(),
    });

    let lut = crate::hdr::ToneLut::build(0.0, crate::hdr::ToneOp::Aces);
    let base = Arc::new(crate::hdr::tonemap(&hdr, &lut));
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

    let hdr = Arc::new(hdr);
    let bytes = Decoded::compute_bytes_with_hdr(std::slice::from_ref(&frame), &mips, Some(&hdr));
    Ok(Decoded {
        hdr: Some(hdr),
        meta,
        frames: vec![frame],
        mips,
        complete: true,
        truncated: false,
        bytes,
    })
}

/// 只讀標頭取得 EXIF 方向
fn jpeg_orientation(path: &Path) -> Orientation {
    open_buffered(path)
        .ok()
        .and_then(|r| JpegDecoder::new(r).ok())
        .and_then(|mut d| d.orientation().ok())
        .unwrap_or(Orientation::NoTransforms)
}

/// 內嵌預覽的最長邊小於此值時，額外做一次完整顯影補上細節。
/// 多數機種內嵌全解析度預覽（此時不會觸發），Sony 等機種只給約 1600px。
const RAW_PREVIEW_ENOUGH: u32 = 2400;

/// RAW 漸進式解碼：
/// 1. 抽內嵌 JPEG 預覽 → 立刻送出，畫面數十毫秒內出現
/// 2. 預覽解析度不足時，背景做完整 demosaic → 送出同一格的高解析度版本取代
///
/// UI 端的 Frame 事件會覆寫同 index 的影格，因此不需要額外協定。
fn decode_raw_progressive(
    path: &Path,
    file_size: u64,
    generation: u64,
    latest_gen: &AtomicU64,
    send: &dyn Fn(LoadEvent),
) -> Result<Decoded, String> {
    let emit = |rgba: RgbaImage, label: &str| -> (ImageMeta, FrameData, Vec<Arc<ColorImage>>) {
        let meta = ImageMeta {
            path: path.to_path_buf(),
            orig_size: [rgba.width(), rgba.height()],
            file_size,
            format: label.to_owned(),
            animated: false,
            has_alpha: false,
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
        (meta, frame, mips)
    };

    let preview = crate::raw_preview::extract(path);
    let (mut meta, mut frame, mut mips) = match preview {
        Some(p) => {
            let enough = p.size.0.max(p.size.1) >= RAW_PREVIEW_ENOUGH;
            let label = if enough { "RAW" } else { "RAW（預覽）" };
            let r = emit(p.image, label);
            if enough {
                // 預覽已是全解析度等級，不必再花 CPU 顯影
                let bytes = Decoded::compute_bytes(std::slice::from_ref(&r.1), &r.2);
                return Ok(Decoded {
                    hdr: None,
                    meta: r.0,
                    frames: vec![r.1],
                    mips: r.2,
                    complete: true,
                    truncated: false,
                    bytes,
                });
            }
            r
        }
        None => {
            // 沒有內嵌預覽：只能直接完整顯影
            let rgba = crate::extra_formats::develop_raw(path)?;
            let r = emit(rgba, "RAW");
            let bytes = Decoded::compute_bytes(std::slice::from_ref(&r.1), &r.2);
            return Ok(Decoded {
                hdr: None,
                meta: r.0,
                frames: vec![r.1],
                mips: r.2,
                complete: true,
                truncated: false,
                bytes,
            });
        }
    };

    // 預覽偏小 → 補上完整顯影。使用者已翻到別張就不浪費 CPU。
    if latest_gen.load(AtomicOrdering::Relaxed) == generation {
        // 顯影失敗（例如 CR3 的原始資料不支援）就維持預覽版本
        if let Ok(rgba) = crate::extra_formats::develop_raw(path) {
            let r = emit(rgba, "RAW（完整顯影）");
            meta = r.0;
            frame = r.1;
            mips = r.2;
        }
    }

    let bytes = Decoded::compute_bytes(std::slice::from_ref(&frame), &mips);
    Ok(Decoded {
        hdr: None,
        meta,
        frames: vec![frame],
        mips,
        complete: true,
        truncated: false,
        bytes,
    })
}

/// 預載：靜態圖全解（含 mip 鏈）；動畫只解第一格，翻到時再全解。
fn decode_prefetch(path: &Path) -> Result<Decoded, String> {
    // HDR 需要保留浮點資料與色調映射，預載路徑不處理；
    // 使用者真的翻到時會由高優先權工作走完整流程（這類檔案本來就少見）
    if is_hdr_file(path) {
        return Err("HDR 不預載".into());
    }
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
        // RAW 預載只抽內嵌預覽（快）；預覽解析度不足時標記為未完成，
        // 使用者真的翻到時才會由高優先權工作補上完整顯影
        let mut complete = true;
        let (rgba, fmt) =
            if crate::extra_formats::sniff(path) == Some(crate::extra_formats::ExtraFormat::Raw) {
                match crate::raw_preview::extract(path) {
                    Some(p) => {
                        let enough = p.size.0.max(p.size.1) >= RAW_PREVIEW_ENOUGH;
                        complete = enough;
                        (
                            p.image,
                            if enough { "RAW" } else { "RAW（預覽）" }.to_owned(),
                        )
                    }
                    None => decode_static(path)?,
                }
            } else {
                decode_static(path)?
            };
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
            hdr: None,
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
            complete,
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
        hdr: None,
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

/// 執行時偵測到的 GPU 貼圖邊長上限。UI 執行緒在每幀更新，
/// 解碼執行緒讀取——用 atomic 避免額外的鎖。
static MAX_TEX_SIDE: AtomicU32 = AtomicU32::new(MAX_TEX_DIM);

/// 由 UI 端告知實際的貼圖上限（`ctx.input(|i| i.max_texture_side)`）
pub fn set_max_tex_side(side: u32) {
    // 太小的值必然是還沒初始化完成，忽略以免把圖縮爛
    if side >= 2048 {
        MAX_TEX_SIDE.store(side, AtomicOrdering::Relaxed);
    }
}

pub fn max_tex_side() -> u32 {
    MAX_TEX_SIDE.load(AtomicOrdering::Relaxed)
}

/// RgbaImage → egui ColorImage；超過 GPU 貼圖上限就逐次減半。
pub fn to_color_image_clamped(mut rgba: RgbaImage) -> ColorImage {
    let max = max_tex_side();
    while rgba.width() > max || rgba.height() > max {
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
