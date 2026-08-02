use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::ColorImage;

/// GPU 貼圖邊長的保守預設值。實際上限在執行時由
/// `ctx.input(|i| i.max_texture_side)` 取得（見 Loader::set_max_tex_side），
/// 這個常數只在還沒問到之前當作起始值。
pub const MAX_TEX_DIM: u32 = 8192;
/// 單一動畫解碼後的記憶體保護上限（超過就停止解碼後續影格）
pub const ANIM_BUDGET_BYTES: usize = 3 * 1024 * 1024 * 1024;
/// 解碼快取總預算（LRU 淘汰）
pub const CACHE_BUDGET_BYTES: usize = 1536 * 1024 * 1024;
/// mip 鏈最小邊長（縮到這個尺寸以下就不再繼續產生）
pub const MIP_MIN_DIM: usize = 1024;

/// 資料夾掃描到的一個圖片檔（含排序用中繼資料；掃描時由目錄項目一次取得）
#[derive(Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub modified: Option<std::time::SystemTime>,
    pub size: u64,
}

/// 動畫的一個影格（靜態圖就是唯一一格，delay 為零）
#[derive(Clone)]
pub struct FrameData {
    pub image: Arc<ColorImage>,
    pub delay: Duration,
    /// 相對「前一格」的變動矩形 `[x, y, w, h]`，供部分貼圖更新使用。
    ///
    /// - `None`：未知，必須整張重傳
    /// - `Some([_, _, 0, 0])`：與前一格完全相同，不必上傳
    ///
    /// 動畫（尤其 GIF）通常每格只有一小塊在變，只傳那一塊可省下大量頻寬。
    pub dirty: Option<[usize; 4]>,
}

impl FrameData {
    /// 一般建構：預設為「整張重傳」
    pub fn new(image: Arc<ColorImage>, delay: Duration) -> Self {
        Self {
            image,
            delay,
            dirty: None,
        }
    }

    pub fn bytes(&self) -> usize {
        self.image.pixels.len() * 4
    }
}

#[derive(Clone, Debug)]
pub struct ImageMeta {
    pub path: PathBuf,
    /// 原始像素尺寸（EXIF 旋轉後；顯示貼圖可能因超過上限而縮小）
    pub orig_size: [u32; 2],
    pub file_size: u64,
    pub format: String,
    pub animated: bool,
    pub has_alpha: bool,
}

/// 解碼完成後放進快取的完整結果
pub struct Decoded {
    pub meta: ImageMeta,
    /// HDR/EXR 的浮點原始資料。保留它才能在調整曝光時
    /// 重新色調映射而不必重新解碼（動畫與一般 8-bit 影像為 None）。
    pub hdr: Option<Arc<crate::hdr::HdrImage>>,
    pub frames: Vec<FrameData>,
    /// 靜態圖的 mip 鏈：[0] 為基底貼圖，之後每層長寬減半（動畫為空）
    pub mips: Vec<Arc<ColorImage>>,
    /// 動畫是否已解碼全部影格（false = 只有部分，例如預載時只解第一格）
    pub complete: bool,
    /// 因記憶體上限或資料損毀而截斷
    pub truncated: bool,
    pub bytes: usize,
}

impl Decoded {
    pub fn compute_bytes(frames: &[FrameData], mips: &[Arc<ColorImage>]) -> usize {
        let f: usize = frames.iter().map(|f| f.bytes()).sum();
        // mips[0] 與 frames[0].image 是同一份 Arc，不重複計算
        let m: usize = mips.iter().skip(1).map(|m| m.pixels.len() * 4).sum();
        f + m
    }

    /// 含 HDR 浮點資料的總記憶體量（LRU 預算要算進去，否則會嚴重低估）
    pub fn compute_bytes_with_hdr(
        frames: &[FrameData],
        mips: &[Arc<ColorImage>],
        hdr: Option<&crate::hdr::HdrImage>,
    ) -> usize {
        Self::compute_bytes(frames, mips) + hdr.map_or(0, |h| h.bytes())
    }
}

/// 背景工作 → UI 執行緒的事件
pub enum LoadEvent {
    /// 檔頭讀到尺寸就先送（可能在任何影格之前）
    Meta {
        generation: u64,
        meta: ImageMeta,
    },
    /// 一個影格解碼完成（動畫會連續送多個；index 從 0 起）
    Frame {
        generation: u64,
        index: usize,
        frame: FrameData,
    },
    /// 靜態圖的 mip 鏈建好了（含基底）
    Mips {
        generation: u64,
        mips: Vec<Arc<ColorImage>>,
    },
    /// 這次載入結束
    Done {
        generation: u64,
        complete: bool,
        truncated: bool,
    },
    Error {
        generation: u64,
        message: String,
    },
    /// 預載完成（UI 若正好在等這張圖可直接採用快取）
    Prefetched {
        path: PathBuf,
    },
    /// 資料夾掃描結果（未排序；UI 依目前排序設定重排）
    DirListing {
        generation: u64,
        entries: Vec<FileEntry>,
    },
}

/// UI → 背景工作的工作項目
pub enum Job {
    /// 使用者正在看的圖（高優先權，動畫全解）
    Load { path: PathBuf, generation: u64 },
    /// 鄰居預載（低優先權；動畫只解第一格）
    Prefetch { path: PathBuf },
    /// 掃描資料夾建立瀏覽清單
    ScanDir { dir: PathBuf, generation: u64 },
}
