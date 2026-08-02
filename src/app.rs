use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Context, Key, PointerButton, Rect, Sense, Vec2};

use crate::cache::Cache;
use crate::dirlist;
use crate::loader::Loader;
use crate::types::*;

const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 64.0;

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum ZoomMode {
    /// 自動適應螢幕（等比縮放到剛好完整可見：大圖縮小、小圖放大）
    Fit,
    /// 填滿裁切（等比放大到塞滿整個視窗，超出部分裁掉，可拖曳查看）
    Cover,
    /// 符合寬度（寬度填滿視窗，可放大；直向長圖從頂部開始）
    FitWidth,
    /// 符合高度（高度填滿視窗，可放大；橫向長圖從左緣開始）
    FitHeight,
    /// 原始大小 1:1
    Actual,
    /// 手動縮放中（滾輪/按鍵調整後的自由值，不會存成偏好）
    Free,
}

impl ZoomMode {
    fn name(self) -> &'static str {
        match self {
            ZoomMode::Fit => "自動適應",
            ZoomMode::Cover => "填滿裁切",
            ZoomMode::FitWidth => "符合寬度",
            ZoomMode::FitHeight => "符合高度",
            ZoomMode::Actual => "原始大小",
            ZoomMode::Free => "自訂",
        }
    }

    fn short_name(self) -> &'static str {
        match self {
            ZoomMode::Fit => "自動",
            ZoomMode::Cover => "填滿",
            ZoomMode::FitWidth => "寬度",
            ZoomMode::FitHeight => "高度",
            ZoomMode::Actual => "1:1",
            ZoomMode::Free => "自訂",
        }
    }
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum BgMode {
    Dark,
    Black,
    Gray,
    White,
}

impl BgMode {
    fn color(self) -> Color32 {
        match self {
            BgMode::Dark => Color32::from_gray(18),
            BgMode::Black => Color32::BLACK,
            BgMode::Gray => Color32::from_gray(90),
            BgMode::White => Color32::from_gray(240),
        }
    }
    fn next(self) -> Self {
        match self {
            BgMode::Dark => BgMode::Black,
            BgMode::Black => BgMode::Gray,
            BgMode::Gray => BgMode::White,
            BgMode::White => BgMode::Dark,
        }
    }
    fn name(self) -> &'static str {
        match self {
            BgMode::Dark => "深色",
            BgMode::Black => "黑",
            BgMode::Gray => "灰",
            BgMode::White => "白",
        }
    }
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum FilterMode {
    Auto,
    Linear,
    Nearest,
}

impl FilterMode {
    fn next(self) -> Self {
        match self {
            FilterMode::Auto => FilterMode::Linear,
            FilterMode::Linear => FilterMode::Nearest,
            FilterMode::Nearest => FilterMode::Auto,
        }
    }
    fn name(self) -> &'static str {
        match self {
            FilterMode::Auto => "自動",
            FilterMode::Linear => "平滑",
            FilterMode::Nearest => "像素",
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Prefs {
    bg: BgMode,
    filter: FilterMode,
    /// 使用者選定的顯示方式（Free 除外），跨圖片與跨啟動保持
    mode: ZoomMode,
    /// 瀏覽排序依據與方向
    sort_key: dirlist::SortKey,
    sort_asc: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            bg: BgMode::Dark,
            filter: FilterMode::Auto,
            mode: ZoomMode::Fit,
            sort_key: dirlist::SortKey::Name,
            sort_asc: true,
        }
    }
}

/// 已建立的 GPU 貼圖（隨 CurrentImage 生滅）
#[derive(Default)]
struct TexCache {
    /// 靜態圖：每個 mip 層一張貼圖，(貼圖, 是否 nearest 放大)
    static_mips: Vec<Option<(egui::TextureHandle, bool)>>,
    /// 動畫：單張貼圖重複更新（避免上百格各佔一份 VRAM）
    anim: Option<egui::TextureHandle>,
    anim_frame: usize,
    anim_nearest: bool,
}

struct CurrentImage {
    meta: ImageMeta,
    frames: Vec<FrameData>,
    mips: Vec<Arc<eframe::egui::ColorImage>>,
    complete: bool,
    truncated: bool,
    tex: TexCache,
}

impl CurrentImage {
    fn empty(meta: ImageMeta) -> Self {
        Self {
            meta,
            frames: Vec::new(),
            mips: Vec::new(),
            complete: false,
            truncated: false,
            tex: TexCache::default(),
        }
    }
    fn from_decoded(d: &Decoded) -> Self {
        Self {
            meta: d.meta.clone(),
            frames: d.frames.clone(),
            mips: d.mips.clone(),
            complete: d.complete,
            truncated: d.truncated,
            tex: TexCache::default(),
        }
    }
    fn is_anim(&self) -> bool {
        self.meta.animated || self.frames.len() > 1
    }
}

pub struct ViewerApp {
    loader: Loader,
    renderer_label: String,

    entries: Vec<FileEntry>,
    index: usize,
    have_dir: bool,

    /// 目前顯示中的影像（一定有可畫的影格）
    current: Option<CurrentImage>,
    /// 正在解碼、尚未可顯示的新影像；解出第一格後才取代 current，
    /// 這段期間畫面持續顯示舊圖，避免切換時出現黑畫面
    incoming: Option<CurrentImage>,
    /// true = 事件屬於 incoming（新圖載入中）
    awaiting: bool,
    /// 新圖開始載入的時間，用來決定何時顯示「載入中」提示
    load_started: Option<Instant>,
    loading_path: Option<PathBuf>,
    error: Option<String>,
    generation: u64,

    mode: ZoomMode,
    zoom: f32,
    pan: Vec2,
    rotation: u8,
    last_effective_zoom: f32,

    playing: bool,
    frame_idx: usize,
    next_frame_at: Option<Instant>,

    wheel_accum: f32,
    last_wheel: Option<Instant>,

    fullscreen: bool,
    show_info: bool,
    prefs: Prefs,
}

impl ViewerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial: Option<PathBuf>,
        renderer_label: String,
    ) -> Self {
        let cache = Arc::new(Mutex::new(Cache::default()));
        let loader = Loader::new(cc.egui_ctx.clone(), cache);
        let prefs = cc
            .storage
            .and_then(|s| eframe::get_value::<Prefs>(s, "prefs"))
            .unwrap_or_default();
        let start_mode = prefs.mode;
        let mut app = Self {
            loader,
            renderer_label,
            entries: Vec::new(),
            index: 0,
            have_dir: false,
            current: None,
            incoming: None,
            awaiting: false,
            load_started: None,
            loading_path: None,
            error: None,
            generation: 0,
            mode: start_mode,
            zoom: 1.0,
            pan: Vec2::ZERO,
            rotation: 0,
            last_effective_zoom: 1.0,
            playing: true,
            frame_idx: 0,
            next_frame_at: None,
            wheel_accum: 0.0,
            last_wheel: None,
            fullscreen: false,
            show_info: false,
            prefs,
        };
        if let Some(p) = initial {
            app.open_path(&cc.egui_ctx, p);
        }
        app
    }

    // -- 導覽與載入 --------------------------------------------------------

    fn open_path(&mut self, ctx: &Context, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        if path.is_dir() {
            // 開資料夾：清掉目前圖片，讓掃描結果直接開啟第一張
            self.generation += 1;
            self.entries = Vec::new();
            self.have_dir = false;
            self.current = None;
            self.incoming = None;
            self.awaiting = false;
            self.load_started = None;
            self.loading_path = None;
            self.error = None;
            self.loader.request_scan(path, self.generation);
            return;
        }

        self.generation += 1;
        self.error = None;
        self.rotation = 0;
        self.apply_mode(self.prefs.mode); // 顯示方式跨圖片保持，手動縮放則還原
        self.loading_path = Some(path.clone());
        self.incoming = None;

        let cached = self.loader.request_current(path.clone(), self.generation);
        match cached {
            // 快取命中（含動畫預載的第一格）：直接換圖，零延遲
            Some(d) if !d.frames.is_empty() => {
                let complete = d.complete;
                self.show(CurrentImage::from_decoded(&d));
                if complete {
                    self.loading_path = None;
                }
            }
            // 尚未解碼：保留畫面上的舊圖，等第一格解出再換，避免黑畫面
            _ => {
                self.awaiting = true;
                self.load_started = Some(Instant::now());
                if self.current.is_none() {
                    self.frame_idx = 0; // 還沒有任何圖可留，播放狀態直接重置
                    self.next_frame_at = None;
                }
            }
        }

        // 資料夾清單：已在清單中就直接更新索引，否則重新掃描
        let in_list = self.have_dir && self.entries.iter().any(|e| e.path == path);
        if in_list {
            if let Some(i) = self.entries.iter().position(|e| e.path == path) {
                self.index = i;
            }
            self.schedule_prefetch();
        } else {
            self.entries = Vec::new();
            self.have_dir = false;
            if let Some(dir) = path.parent() {
                self.loader.request_scan(dir.to_path_buf(), self.generation);
            }
        }
        self.update_title(ctx);
    }

    /// 把影像換到畫面上（重置播放狀態），並結束等待中的載入
    fn show(&mut self, img: CurrentImage) {
        self.current = Some(img);
        self.incoming = None;
        self.awaiting = false;
        self.load_started = None;
        self.frame_idx = 0;
        self.next_frame_at = None;
        self.playing = true;
    }

    /// 載入中的新圖已有可畫的影格 → 取代畫面上的舊圖
    fn promote_if_ready(&mut self, ctx: &Context) {
        let ready = self.incoming.as_ref().is_some_and(|c| !c.frames.is_empty());
        if ready {
            let img = self.incoming.take().unwrap();
            self.show(img);
            self.update_title(ctx);
        }
    }

    fn nav(&mut self, ctx: &Context, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as isize - 1;
        let new = (self.index as isize + delta).clamp(0, last) as usize;
        if new != self.index {
            self.index = new;
            let p = self.entries[new].path.clone();
            self.open_path(ctx, p);
        }
    }

    fn nav_to(&mut self, ctx: &Context, idx: usize) {
        if idx < self.entries.len() && idx != self.index {
            self.index = idx;
            let p = self.entries[idx].path.clone();
            self.open_path(ctx, p);
        }
    }

    /// 依目前排序設定重排清單，跟住目前圖片的位置並更新預載
    fn apply_sort(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        dirlist::sort_entries(&mut self.entries, self.prefs.sort_key, self.prefs.sort_asc);
        let cur = self
            .current
            .as_ref()
            .map(|c| c.meta.path.clone())
            .or_else(|| self.loading_path.clone());
        if let Some(p) = cur {
            if let Some(i) = self.entries.iter().position(|e| e.path == p) {
                self.index = i;
            }
        }
        self.schedule_prefetch();
    }

    fn schedule_prefetch(&self) {
        if self.entries.is_empty() {
            return;
        }
        let mut protect = Vec::new();
        if let Some(c) = &self.current {
            protect.push(c.meta.path.clone());
        }
        if let Some(p) = &self.loading_path {
            protect.push(p.clone());
        }
        for off in [1isize, -1, 2, -2] {
            let i = self.index as isize + off;
            if i >= 0 && (i as usize) < self.entries.len() {
                let p = self.entries[i as usize].path.clone();
                protect.push(p.clone());
                self.loader.request_prefetch(p);
            }
        }
        self.loader.set_protected(protect);
    }

    fn reload_current(&mut self, ctx: &Context) {
        let path = self
            .current
            .as_ref()
            .map(|c| c.meta.path.clone())
            .or_else(|| self.loading_path.clone());
        if let Some(p) = path {
            self.loader.evict(&p);
            self.open_path(ctx, p);
        }
    }

    fn open_dialog(&mut self, ctx: &Context) {
        let mut fd = rfd::FileDialog::new()
            .add_filter("圖片", &dirlist::EXTS)
            .add_filter("所有檔案", &["*"]);
        if let Some(dir) = self
            .current
            .as_ref()
            .and_then(|c| c.meta.path.parent().map(|p| p.to_path_buf()))
        {
            fd = fd.set_directory(dir);
        }
        if let Some(p) = fd.pick_file() {
            self.open_path(ctx, p);
        }
    }

    fn update_title(&self, ctx: &Context) {
        let name = self
            .current
            .as_ref()
            .map(|c| c.meta.path.clone())
            .or_else(|| self.loading_path.clone())
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let title = if name.is_empty() {
            "Zoetrope".to_owned()
        } else {
            format!("{name} — Zoetrope")
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    // -- 背景事件 ----------------------------------------------------------

    fn process_events(&mut self, ctx: &Context) {
        let events: Vec<LoadEvent> = self.loader.events.try_iter().collect();
        for ev in events {
            match ev {
                LoadEvent::Meta { generation, meta } if generation == self.generation => {
                    // 載入中的新圖寫進 incoming，畫面上的舊圖不受影響
                    let slot = if self.awaiting {
                        &mut self.incoming
                    } else {
                        &mut self.current
                    };
                    match slot {
                        Some(c) => c.meta = meta,
                        None => *slot = Some(CurrentImage::empty(meta)),
                    }
                    if !self.awaiting {
                        self.update_title(ctx);
                    }
                }
                LoadEvent::Frame {
                    generation,
                    index,
                    frame,
                } if generation == self.generation => {
                    let slot = if self.awaiting {
                        &mut self.incoming
                    } else {
                        &mut self.current
                    };
                    let c = match slot {
                        Some(c) => c,
                        None => continue, // Meta 一定先到；防禦性略過
                    };
                    if index < c.frames.len() {
                        c.frames[index] = frame;
                    } else if index == c.frames.len() {
                        c.frames.push(frame);
                    }
                    self.promote_if_ready(ctx);
                }
                LoadEvent::Mips { generation, mips } if generation == self.generation => {
                    let slot = if self.awaiting {
                        &mut self.incoming
                    } else {
                        &mut self.current
                    };
                    if let Some(c) = slot {
                        c.mips = mips;
                        c.tex.static_mips.clear();
                    }
                }
                LoadEvent::Done {
                    generation,
                    complete,
                    truncated,
                } if generation == self.generation => {
                    let slot = if self.awaiting {
                        &mut self.incoming
                    } else {
                        &mut self.current
                    };
                    if let Some(c) = slot {
                        c.complete = complete;
                        c.truncated = truncated;
                    }
                    // 解碼結束仍在等待 → 換上（防禦：正常情況第一格早已觸發）
                    self.promote_if_ready(ctx);
                    if self.awaiting {
                        // 沒有任何影格可顯示，結束等待狀態
                        self.awaiting = false;
                        self.incoming = None;
                        self.load_started = None;
                    }
                    self.loading_path = None;
                    self.schedule_prefetch();
                }
                LoadEvent::Error {
                    generation,
                    message,
                } if generation == self.generation => {
                    self.error = Some(message);
                    self.loading_path = None;
                    if self.awaiting {
                        // 這張開不起來：清掉停留的舊畫面，顯示錯誤
                        self.awaiting = false;
                        self.incoming = None;
                        self.load_started = None;
                        self.current = None;
                        self.update_title(ctx);
                    }
                }
                LoadEvent::Prefetched { path } => {
                    // 使用者正好翻到還沒解完的這張：直接採用快取的預載結果。
                    // 動畫的完整解碼已由 open_path 排入高優先佇列，這裡不再重複請求。
                    let waiting =
                        self.awaiting && self.loading_path.as_ref().is_some_and(|p| *p == path);
                    if waiting {
                        if let Some(d) = self.loader.peek(&path) {
                            if !d.frames.is_empty() {
                                let complete = d.complete;
                                self.show(CurrentImage::from_decoded(&d));
                                if complete {
                                    self.loading_path = None;
                                }
                                self.update_title(ctx);
                            }
                        }
                    }
                }
                LoadEvent::DirListing {
                    generation: _,
                    mut entries,
                } => {
                    dirlist::sort_entries(&mut entries, self.prefs.sort_key, self.prefs.sort_asc);
                    // 只在清單包含目前圖片（或還沒有圖片）時採用，避免舊掃描覆蓋新狀態
                    let cur_path = self
                        .current
                        .as_ref()
                        .map(|c| c.meta.path.clone())
                        .or_else(|| self.loading_path.clone());
                    match cur_path {
                        Some(p) => {
                            if let Some(i) = entries.iter().position(|e| e.path == p) {
                                self.entries = entries;
                                self.index = i;
                                self.have_dir = true;
                                self.schedule_prefetch();
                            }
                        }
                        None => {
                            if !entries.is_empty() {
                                self.entries = entries;
                                self.index = 0;
                                self.have_dir = true;
                                let p = self.entries[0].path.clone();
                                self.open_path(ctx, p);
                            }
                        }
                    }
                }
                _ => {} // 過時 generation 的事件直接丟棄
            }
        }
    }

    // -- 動畫播放 ----------------------------------------------------------

    fn advance_animation(&mut self, ctx: &Context) {
        let Some(cur) = &self.current else { return };
        let n = cur.frames.len();
        if n == 0 {
            return;
        }
        if self.frame_idx >= n {
            self.frame_idx = n - 1;
        }
        let streaming = cur.is_anim() && !cur.complete;
        if (n <= 1 && !streaming) || !self.playing {
            return;
        }
        let now = Instant::now();
        let mut next_at = match self.next_frame_at {
            Some(t) => t,
            None => now + cur.frames[self.frame_idx].delay,
        };
        // 視窗凍結／系統休眠後不要瘋狂快轉
        if now > next_at + Duration::from_millis(1000) {
            next_at = now;
        }
        while now >= next_at {
            let next = self.frame_idx + 1;
            if next < n {
                self.frame_idx = next;
            } else if cur.complete {
                self.frame_idx = 0;
            } else {
                // 播放追上解碼進度：暫停計時，新影格到達時會重新排程
                self.next_frame_at = None;
                return;
            }
            next_at += cur.frames[self.frame_idx].delay;
        }
        self.next_frame_at = Some(next_at);
        ctx.request_repaint_after(next_at - now);
    }

    fn step_frame(&mut self, delta: isize) {
        let Some(cur) = &self.current else { return };
        let n = cur.frames.len();
        if n <= 1 {
            return;
        }
        self.playing = false;
        self.next_frame_at = None;
        let i = self.frame_idx as isize + delta;
        self.frame_idx = i.rem_euclid(n as isize) as usize;
    }

    // -- 輸入 --------------------------------------------------------------

    fn handle_input(&mut self, ctx: &Context) {
        // 拖放檔案
        let dropped: Option<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
        if let Some(p) = dropped {
            self.open_path(ctx, p);
        }

        struct Keys {
            next: bool,
            prev: bool,
            home: bool,
            end: bool,
            space: bool,
            zoom_in: bool,
            zoom_out: bool,
            fit: bool,
            cover: bool,
            fit_w: bool,
            fit_h: bool,
            actual: bool,
            double: bool,
            rot_cw: bool,
            rot_ccw: bool,
            fullscreen: bool,
            escape: bool,
            info: bool,
            bg: bool,
            filter: bool,
            open: bool,
            reload: bool,
            sort_flip: bool,
            step_fwd: bool,
            step_back: bool,
            mouse_back: bool,
            mouse_fwd: bool,
        }
        let k = ctx.input(|i| Keys {
            next: i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::PageDown),
            prev: i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::PageUp),
            home: i.key_pressed(Key::Home),
            end: i.key_pressed(Key::End),
            space: i.key_pressed(Key::Space),
            zoom_in: i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
            zoom_out: i.key_pressed(Key::Minus),
            fit: i.key_pressed(Key::Num0),
            cover: i.key_pressed(Key::Num3),
            fit_w: i.key_pressed(Key::W),
            fit_h: i.key_pressed(Key::H),
            actual: i.key_pressed(Key::Num1),
            double: i.key_pressed(Key::Num2),
            rot_cw: i.key_pressed(Key::R) && !i.modifiers.shift,
            rot_ccw: i.key_pressed(Key::R) && i.modifiers.shift,
            fullscreen: i.key_pressed(Key::F) || i.key_pressed(Key::F11),
            escape: i.key_pressed(Key::Escape),
            info: i.key_pressed(Key::I),
            bg: i.key_pressed(Key::B),
            filter: i.key_pressed(Key::N),
            open: i.key_pressed(Key::O),
            reload: i.key_pressed(Key::F5),
            sort_flip: i.key_pressed(Key::S),
            step_fwd: i.key_pressed(Key::Period),
            step_back: i.key_pressed(Key::Comma),
            mouse_back: i.pointer.button_pressed(PointerButton::Extra1),
            mouse_fwd: i.pointer.button_pressed(PointerButton::Extra2),
        });

        if k.next || k.mouse_fwd {
            self.nav(ctx, 1);
        }
        if k.prev || k.mouse_back {
            self.nav(ctx, -1);
        }
        if k.home {
            self.nav_to(ctx, 0);
        }
        if k.end && !self.entries.is_empty() {
            let last = self.entries.len() - 1;
            self.nav_to(ctx, last);
        }
        if k.sort_flip {
            self.prefs.sort_asc = !self.prefs.sort_asc;
            self.apply_sort();
        }
        if k.space {
            let animated = self.current.as_ref().is_some_and(|c| c.frames.len() > 1);
            if animated {
                self.playing = !self.playing;
                self.next_frame_at = None;
            } else {
                self.nav(ctx, 1);
            }
        }
        if k.zoom_in {
            self.zoom_step(ctx, 1.25);
        }
        if k.zoom_out {
            self.zoom_step(ctx, 1.0 / 1.25);
        }
        if k.fit {
            self.apply_mode(ZoomMode::Fit);
        }
        if k.cover {
            self.apply_mode(ZoomMode::Cover);
        }
        if k.fit_w {
            self.apply_mode(ZoomMode::FitWidth);
        }
        if k.fit_h {
            self.apply_mode(ZoomMode::FitHeight);
        }
        if k.actual {
            self.apply_mode(ZoomMode::Actual);
        }
        if k.double {
            self.set_zoom_anchored(2.0, None, ctx);
        }
        if k.rot_cw {
            self.rotation = (self.rotation + 1) % 4;
        }
        if k.rot_ccw {
            self.rotation = (self.rotation + 3) % 4;
        }
        if k.fullscreen {
            self.toggle_fullscreen(ctx);
        }
        if k.escape {
            if self.fullscreen {
                self.toggle_fullscreen(ctx);
            } else if self.show_info {
                self.show_info = false;
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if k.info {
            self.show_info = !self.show_info;
        }
        if k.bg {
            self.prefs.bg = self.prefs.bg.next();
        }
        if k.filter {
            self.prefs.filter = self.prefs.filter.next();
        }
        if k.open {
            self.open_dialog(ctx);
        }
        if k.reload {
            self.reload_current(ctx);
        }
        if k.step_fwd {
            self.step_frame(1);
        }
        if k.step_back {
            self.step_frame(-1);
        }
    }

    fn toggle_fullscreen(&mut self, ctx: &Context) {
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    // -- 縮放與幾何 --------------------------------------------------------

    fn rotated_dims(&self) -> Option<Vec2> {
        let c = self.current.as_ref()?;
        let [w, h] = c.meta.orig_size;
        let (w, h) = (w as f32, h as f32);
        Some(if self.rotation % 2 == 1 {
            Vec2::new(h, w)
        } else {
            Vec2::new(w, h)
        })
    }

    /// 切換顯示方式並設定起始平移；Free 以外的模式會存成偏好。
    fn apply_mode(&mut self, mode: ZoomMode) {
        self.mode = mode;
        if mode != ZoomMode::Free {
            self.prefs.mode = mode;
        }
        // 起始位置：寬度模式看頂部、高度模式看左緣（clamp_pan 每幀會夾回合法範圍）
        self.pan = match mode {
            ZoomMode::FitWidth => Vec2::new(0.0, 1e9),
            ZoomMode::FitHeight => Vec2::new(1e9, 0.0),
            _ => Vec2::ZERO,
        };
    }

    fn effective_zoom(&self, avail: Vec2, ppp: f32) -> f32 {
        let Some(dims) = self.rotated_dims() else {
            return 1.0;
        };
        match self.mode {
            // 等比縮放至剛好完整可見：大圖縮小、小圖放大，貼合視窗
            ZoomMode::Fit => ((avail.x * ppp) / dims.x).min((avail.y * ppp) / dims.y),
            // 等比縮放到塞滿視窗（取較大比例），超出部分裁掉
            ZoomMode::Cover => ((avail.x * ppp) / dims.x).max((avail.y * ppp) / dims.y),
            ZoomMode::FitWidth => (avail.x * ppp) / dims.x,
            ZoomMode::FitHeight => (avail.y * ppp) / dims.y,
            ZoomMode::Actual => 1.0,
            ZoomMode::Free => self.zoom,
        }
    }

    /// 以 anchor（螢幕座標，None = 畫面中心）為支點縮放到 target 倍率
    fn set_zoom_anchored(&mut self, target: f32, anchor: Option<egui::Pos2>, ctx: &Context) {
        let avail = ctx.available_rect();
        let ppp = ctx.pixels_per_point();
        let cur = self.effective_zoom(avail.size(), ppp);
        let target = target.clamp(MIN_ZOOM, MAX_ZOOM);
        if (target - cur).abs() < f32::EPSILON {
            return;
        }
        let factor = target / cur;
        let anchor = anchor.unwrap_or_else(|| avail.center());
        let center = avail.center() + self.pan;
        let offset = anchor - center;
        self.pan += offset * (1.0 - factor);
        self.zoom = target;
        self.mode = ZoomMode::Free;
    }

    fn zoom_step(&mut self, ctx: &Context, factor: f32) {
        let avail = ctx.available_rect();
        let ppp = ctx.pixels_per_point();
        let cur = self.effective_zoom(avail.size(), ppp);
        self.set_zoom_anchored(cur * factor, None, ctx);
    }

    fn clamp_pan(pan: Vec2, img_size: Vec2, avail: Vec2) -> Vec2 {
        let clamp_axis = |p: f32, img: f32, avail: f32| {
            if img <= avail {
                0.0
            } else {
                let max = (img - avail) / 2.0;
                p.clamp(-max, max)
            }
        };
        Vec2::new(
            clamp_axis(pan.x, img_size.x, avail.x),
            clamp_axis(pan.y, img_size.y, avail.y),
        )
    }

    // -- 繪製 --------------------------------------------------------------

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let painter = ui.painter().with_clip_rect(rect);

        // 滾輪＝切換上下張（Ctrl+滾輪／捏合才是縮放，egui 已把帶 Ctrl 的滾動
        // 轉成 zoom_delta，不會出現在 smooth_scroll_delta）。
        // 累加器讓一格滾輪剛好翻一張，觸控板則需要一小段滑動距離。
        if response.hovered() {
            let scroll_y = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll_y != 0.0 {
                let now = Instant::now();
                let stale = self
                    .last_wheel
                    .is_none_or(|t| now.duration_since(t) > Duration::from_millis(300));
                if stale {
                    self.wheel_accum = 0.0;
                }
                self.last_wheel = Some(now);
                let steps = wheel_steps(&mut self.wheel_accum, scroll_y);
                if steps != 0 {
                    // nav 之後照常往下畫（此時 self.current 已是新圖或維持舊圖），
                    // 不能提早 return，否則這一幀會是空白 → 視覺上閃爍
                    self.nav(&ctx, steps);
                }
            }
        }

        // 中鍵單擊＝切換全螢幕（clicked_by 會排除拖曳，中鍵拖曳仍是平移）。
        // 放在畫面狀態判斷之前，空畫面／載入中也能切換。
        if response.clicked_by(PointerButton::Middle) {
            self.toggle_fullscreen(&ctx);
        }

        let Some(cur) = &self.current else {
            // 還沒有任何圖可顯示：載入中顯示進度，否則顯示歡迎畫面
            if self.awaiting || self.loading_path.is_some() || self.error.is_some() {
                self.draw_loading(ui, rect);
            } else {
                self.draw_empty_state(ui, rect);
            }
            return;
        };

        if cur.frames.is_empty() {
            self.draw_loading(ui, rect);
            return;
        }

        // --- 幾何 ---
        let dims = self.rotated_dims().unwrap_or(Vec2::splat(1.0));
        let zoom = self.effective_zoom(rect.size(), ppp);

        // --- 互動：拖曳平移 / 滾輪縮放 / 雙擊切換 ---
        if response.dragged_by(PointerButton::Primary) || response.dragged_by(PointerButton::Middle)
        {
            // Fit 模式圖片必定完整可見，多餘位移由 clamp_pan 夾回
            self.pan += response.drag_delta();
        }
        // 縮放：觸控板捏合或 Ctrl+滾輪
        let (factor, hover_pos) = ctx.input(|i| (i.zoom_delta(), i.pointer.hover_pos()));
        if (factor - 1.0).abs() > 1e-5 {
            let cur_zoom = zoom;
            let target = (cur_zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
            let anchor = hover_pos.unwrap_or_else(|| rect.center());
            let center = rect.center() + self.pan;
            let offset = anchor - center;
            self.pan += offset * (1.0 - target / cur_zoom);
            self.zoom = target;
            self.mode = ZoomMode::Free;
        }
        if response.double_clicked() {
            let anchor = response.interact_pointer_pos();
            // 已在 1:1（不論是 Actual 模式或手動縮到 100%）→ 回到選定的顯示方式
            let at_actual = self.mode == ZoomMode::Actual
                || (self.mode == ZoomMode::Free && (zoom - 1.0).abs() < 0.001);
            if at_actual {
                let back = if self.prefs.mode == ZoomMode::Actual {
                    ZoomMode::Fit
                } else {
                    self.prefs.mode
                };
                self.apply_mode(back);
            } else {
                self.set_zoom_anchored(1.0, anchor, &ctx);
            }
        }

        // 重新取得（互動可能改變）縮放與尺寸
        let zoom = self.effective_zoom(rect.size(), ppp);
        self.last_effective_zoom = zoom;
        let size_pts = {
            let s = dims * zoom / ppp;
            // 對齊實體像素格，避免 1:1 檢視時模糊
            Vec2::new((s.x * ppp).round() / ppp, (s.y * ppp).round() / ppp)
        };
        self.pan = Self::clamp_pan(self.pan, size_pts, rect.size());
        let center = rect.center() + self.pan;
        let img_rect = Rect::from_center_size(
            egui::pos2(
                (center.x * ppp).round() / ppp,
                (center.y * ppp).round() / ppp,
            ),
            size_pts,
        );

        // 平移游標提示
        let pannable = size_pts.x > rect.width() || size_pts.y > rect.height();
        if pannable {
            if response.dragged() {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if response.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::Grab);
            }
        }

        // --- 選貼圖並繪製 ---
        let nearest = match self.prefs.filter {
            // 只在使用者手動放大（自訂縮放）時自動切換像素取樣；
            // 顯示模式自動放大的小圖仍用平滑取樣
            FilterMode::Auto => zoom >= 4.0 && self.mode == ZoomMode::Free,
            FilterMode::Linear => false,
            FilterMode::Nearest => true,
        };
        let cur = self.current.as_mut().unwrap();
        let tex_id = Self::pick_texture(&ctx, cur, self.frame_idx, zoom, nearest);

        if let Some(tex_id) = tex_id {
            let uv: [[f32; 2]; 4] = match self.rotation % 4 {
                0 => [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                1 => [[0.0, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
                2 => [[1.0, 1.0], [0.0, 1.0], [0.0, 0.0], [1.0, 0.0]],
                _ => [[1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]],
            };
            let corners = [
                img_rect.left_top(),
                img_rect.right_top(),
                img_rect.right_bottom(),
                img_rect.left_bottom(),
            ];
            let mut mesh = egui::Mesh::with_texture(tex_id);
            for (c, u) in corners.iter().zip(uv) {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: *c,
                    uv: egui::pos2(u[0], u[1]),
                    color: Color32::WHITE,
                });
            }
            mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
            painter.add(egui::Shape::mesh(mesh));
        }

        // --- 角落狀態提示 ---
        let cur = self.current.as_ref().unwrap();
        let mut notes: Vec<String> = Vec::new();
        // 舊圖續留超過 200ms 才提示，快速切換時完全安靜
        if self.awaiting
            && self
                .load_started
                .is_some_and(|t| t.elapsed() > Duration::from_millis(200))
        {
            let name = self
                .loading_path
                .as_ref()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_default();
            notes.push(format!("載入中… {name}"));
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if cur.is_anim() && !cur.complete {
            notes.push(format!("串流解碼中…已 {} 格", cur.frames.len()));
        }
        if cur.truncated {
            notes.push("⚠ 動畫過大，僅載入部分影格".into());
        }
        if let Some(err) = &self.error {
            notes.push(format!("⚠ {err}"));
        }
        for (i, n) in notes.iter().enumerate() {
            painter.text(
                rect.left_top() + Vec2::new(10.0, 10.0 + i as f32 * 20.0),
                egui::Align2::LEFT_TOP,
                n,
                egui::FontId::proportional(13.0),
                Color32::from_rgba_premultiplied(255, 255, 255, 180),
            );
        }
    }

    /// 挑選要用的貼圖：動畫用單張重複更新；靜態圖依縮放挑 mip 層
    fn pick_texture(
        ctx: &Context,
        cur: &mut CurrentImage,
        frame_idx: usize,
        zoom: f32,
        nearest: bool,
    ) -> Option<egui::TextureId> {
        let opts = |nearest: bool| egui::TextureOptions {
            magnification: if nearest {
                egui::TextureFilter::Nearest
            } else {
                egui::TextureFilter::Linear
            },
            ..egui::TextureOptions::LINEAR
        };

        if cur.frames.len() > 1 {
            let fi = frame_idx.min(cur.frames.len() - 1);
            let img = cur.frames[fi].image.clone();
            match &mut cur.tex.anim {
                None => {
                    let tex = ctx.load_texture("anim", egui::ImageData::Color(img), opts(nearest));
                    cur.tex.anim_frame = fi;
                    cur.tex.anim_nearest = nearest;
                    cur.tex.anim = Some(tex);
                }
                Some(tex) => {
                    if cur.tex.anim_frame != fi || cur.tex.anim_nearest != nearest {
                        tex.set(egui::ImageData::Color(img), opts(nearest));
                        cur.tex.anim_frame = fi;
                        cur.tex.anim_nearest = nearest;
                    }
                }
            }
            return cur.tex.anim.as_ref().map(|t| t.id());
        }

        // 靜態圖：mips 還沒好之前先用第一格當基底
        let mips: &[Arc<eframe::egui::ColorImage>] = if cur.mips.is_empty() {
            std::slice::from_ref(&cur.frames[0].image)
        } else {
            &cur.mips
        };
        // 基底貼圖相對原圖的比例（超大圖會被縮小上傳）
        let s0 = mips[0].size[0] as f32 / cur.meta.orig_size[0].max(1) as f32;
        let mut level = 0usize;
        if zoom < s0 {
            level = (s0 / zoom).log2().floor() as usize;
        }
        level = level.min(mips.len() - 1);

        if cur.tex.static_mips.len() != mips.len() {
            cur.tex.static_mips.resize_with(mips.len(), || None);
        }
        let slot = &mut cur.tex.static_mips[level];
        let needs = match slot {
            Some((_, n)) => *n != nearest,
            None => true,
        };
        if needs {
            let tex = ctx.load_texture(
                format!("mip{level}"),
                egui::ImageData::Color(mips[level].clone()),
                opts(nearest),
            );
            *slot = Some((tex, nearest));
        }
        slot.as_ref().map(|(t, _)| t.id())
    }

    fn draw_empty_state(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let painter = ui.painter().clone();
        painter.text(
            rect.center() - Vec2::new(0.0, 40.0),
            egui::Align2::CENTER_CENTER,
            "拖曳圖片到視窗，或按 Ctrl+O 開啟",
            egui::FontId::proportional(20.0),
            ui.visuals().weak_text_color(),
        );
        painter.text(
            rect.center() - Vec2::new(0.0, 12.0),
            egui::Align2::CENTER_CENTER,
            "支援 JPEG · PNG/APNG · GIF · WebP · BMP · TIFF · TGA · QOI · EXR …",
            egui::FontId::proportional(13.0),
            ui.visuals().weak_text_color().gamma_multiply(0.7),
        );
        let btn_rect =
            Rect::from_center_size(rect.center() + Vec2::new(0.0, 36.0), Vec2::new(150.0, 34.0));
        if ui.put(btn_rect, egui::Button::new("開啟圖片…")).clicked() {
            let ctx = ui.ctx().clone();
            self.open_dialog(&ctx);
        }
        if let Some(err) = &self.error {
            painter.text(
                rect.center() + Vec2::new(0.0, 80.0),
                egui::Align2::CENTER_CENTER,
                format!("無法載入：{err}"),
                egui::FontId::proportional(14.0),
                Color32::from_rgb(240, 120, 110),
            );
        }
    }

    fn draw_loading(&self, ui: &mut egui::Ui, rect: Rect) {
        if let Some(err) = &self.error {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("無法載入：{err}"),
                egui::FontId::proportional(15.0),
                Color32::from_rgb(240, 120, 110),
            );
            return;
        }
        ui.put(
            Rect::from_center_size(rect.center(), Vec2::splat(44.0)),
            egui::Spinner::new().size(36.0),
        );
        let label = self
            .incoming
            .as_ref()
            .or(self.current.as_ref())
            .map(|c| {
                format!(
                    "解碼中… {} ({}×{})",
                    c.meta
                        .path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    c.meta.orig_size[0],
                    c.meta.orig_size[1]
                )
            })
            .or_else(|| {
                self.loading_path.as_ref().map(|p| {
                    format!(
                        "讀取中… {}",
                        p.file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    )
                })
            })
            .unwrap_or_default();
        ui.painter().text(
            rect.center() + Vec2::new(0.0, 44.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(14.0),
            ui.visuals().weak_text_color(),
        );
    }

    fn toolbar(&mut self, ctx: &Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let mut act_open = false;
                let mut act_prev = false;
                let mut act_next = false;
                let mut act_zoom: Option<f32> = None;
                let mut act_mode: Option<ZoomMode> = None;
                let mut act_rot_ccw = false;
                let mut act_rot_cw = false;
                let mut act_play = false;
                let mut act_full = false;

                if ui
                    .button("開啟")
                    .on_hover_text("開啟圖片 (Ctrl+O / O)")
                    .clicked()
                {
                    act_open = true;
                }
                ui.separator();
                let has_files = !self.entries.is_empty();
                if ui
                    .add_enabled(has_files && self.index > 0, egui::Button::new("◀"))
                    .on_hover_text("上一張 (←)")
                    .clicked()
                {
                    act_prev = true;
                }
                if ui
                    .add_enabled(
                        has_files && self.index + 1 < self.entries.len(),
                        egui::Button::new("▶"),
                    )
                    .on_hover_text("下一張 (→)")
                    .clicked()
                {
                    act_next = true;
                }
                // 排序選單：依據 + 升降序
                let mut sort_changed = false;
                ui.menu_button("排序", |ui| {
                    for key in [
                        dirlist::SortKey::Name,
                        dirlist::SortKey::Modified,
                        dirlist::SortKey::Size,
                        dirlist::SortKey::Type,
                    ] {
                        if ui
                            .radio_value(&mut self.prefs.sort_key, key, key.name())
                            .changed()
                        {
                            sort_changed = true;
                        }
                    }
                    ui.separator();
                    if ui
                        .radio_value(&mut self.prefs.sort_asc, true, "升序")
                        .changed()
                    {
                        sort_changed = true;
                    }
                    if ui
                        .radio_value(&mut self.prefs.sort_asc, false, "降序")
                        .changed()
                    {
                        sort_changed = true;
                    }
                })
                .response
                .on_hover_text(format!(
                    "排序：{} · {}（S 鍵快速切換升降序）",
                    self.prefs.sort_key.name(),
                    if self.prefs.sort_asc {
                        "升序"
                    } else {
                        "降序"
                    }
                ));
                if sort_changed {
                    self.apply_sort();
                }
                ui.separator();
                if ui.button("−").on_hover_text("縮小 (-)").clicked() {
                    act_zoom = Some(1.0 / 1.25);
                }
                ui.label(format!("{:.0}%", self.last_effective_zoom * 100.0));
                if ui.button("+").on_hover_text("放大 (+)").clicked() {
                    act_zoom = Some(1.25);
                }
                // 顯示方式選單（按鈕顯示目前模式，點開選擇）
                ui.menu_button(self.mode.short_name(), |ui| {
                    for m in [
                        ZoomMode::Fit,
                        ZoomMode::Cover,
                        ZoomMode::FitWidth,
                        ZoomMode::FitHeight,
                        ZoomMode::Actual,
                    ] {
                        if ui.radio(self.mode == m, m.name()).clicked() {
                            act_mode = Some(m);
                            ui.close_menu();
                        }
                    }
                })
                .response
                .on_hover_text(format!("顯示方式：{}（快捷鍵 0/W/H/1）", self.mode.name()));
                ui.separator();
                if ui
                    .button("⟲")
                    .on_hover_text("逆時針旋轉 (Shift+R)")
                    .clicked()
                {
                    act_rot_ccw = true;
                }
                if ui.button("⟳").on_hover_text("順時針旋轉 (R)").clicked() {
                    act_rot_cw = true;
                }

                let animated = self.current.as_ref().is_some_and(|c| c.frames.len() > 1);
                if animated {
                    ui.separator();
                    let label = if self.playing { "⏸" } else { "▶" };
                    if ui
                        .button(label)
                        .on_hover_text("播放／暫停 (Space)")
                        .clicked()
                    {
                        act_play = true;
                    }
                    if let Some(c) = &self.current {
                        ui.label(format!("{}/{}", self.frame_idx + 1, c.frames.len()));
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⛶").on_hover_text("全螢幕 (F / F11)").clicked() {
                        act_full = true;
                    }
                    if ui
                        .selectable_label(self.show_info, "ℹ")
                        .on_hover_text("圖片資訊 (I)")
                        .clicked()
                    {
                        self.show_info = !self.show_info;
                    }
                });

                if act_open {
                    self.open_dialog(ctx);
                }
                if act_prev {
                    self.nav(ctx, -1);
                }
                if act_next {
                    self.nav(ctx, 1);
                }
                if let Some(f) = act_zoom {
                    self.zoom_step(ctx, f);
                }
                if let Some(m) = act_mode {
                    self.apply_mode(m);
                }
                if act_rot_ccw {
                    self.rotation = (self.rotation + 3) % 4;
                }
                if act_rot_cw {
                    self.rotation = (self.rotation + 1) % 4;
                }
                if act_play {
                    self.playing = !self.playing;
                    self.next_frame_at = None;
                }
                if act_full {
                    self.toggle_fullscreen(ctx);
                }
            });
        });
    }

    fn status_bar(&mut self, ctx: &Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let mut left = String::new();
                if !self.entries.is_empty() {
                    left.push_str(&format!("{} / {}", self.index + 1, self.entries.len()));
                }
                if let Some(c) = &self.current {
                    if !left.is_empty() {
                        left.push_str("  ·  ");
                    }
                    left.push_str(&format!(
                        "{}  ·  {}×{}  ·  {}  ·  {}",
                        c.meta
                            .path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        c.meta.orig_size[0],
                        c.meta.orig_size[1],
                        fmt_bytes(c.meta.file_size as usize),
                        c.meta.format,
                    ));
                }
                ui.label(egui::RichText::new(left).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{:.0}%  ·  {}  ·  背景:{}  ·  取樣:{}",
                            self.last_effective_zoom * 100.0,
                            self.mode.name(),
                            self.prefs.bg.name(),
                            self.prefs.filter.name(),
                        ))
                        .size(12.0),
                    );
                });
            });
        });
    }

    fn info_window(&mut self, ctx: &Context) {
        if !self.show_info {
            return;
        }
        let mut open = self.show_info;
        egui::Window::new("圖片資訊")
            .open(&mut open)
            .anchor(egui::Align2::RIGHT_TOP, [-12.0, 48.0])
            .resizable(false)
            .show(ctx, |ui| {
                if let Some(c) = &self.current {
                    egui::Grid::new("info-grid").num_columns(2).show(ui, |ui| {
                        ui.label("檔名");
                        ui.label(
                            c.meta
                                .path
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                        );
                        ui.end_row();
                        ui.label("尺寸");
                        ui.label(format!("{} × {}", c.meta.orig_size[0], c.meta.orig_size[1]));
                        ui.end_row();
                        ui.label("檔案大小");
                        ui.label(fmt_bytes(c.meta.file_size as usize));
                        ui.end_row();
                        ui.label("格式");
                        ui.label(&c.meta.format);
                        ui.end_row();
                        if c.frames.len() > 1 {
                            ui.label("影格");
                            ui.label(format!(
                                "{}{}",
                                c.frames.len(),
                                if c.complete { "" } else { "（解碼中）" }
                            ));
                            ui.end_row();
                        }
                        ui.label("透明通道");
                        ui.label(if c.meta.has_alpha { "有" } else { "無" });
                        ui.end_row();
                        let (used, count) = self.loader.cache_stats();
                        ui.label("快取");
                        ui.label(format!("{} · {} 張", fmt_bytes(used), count));
                        ui.end_row();
                        ui.label("繪圖後端");
                        ui.label(&self.renderer_label);
                        ui.end_row();
                        ui.label("貼圖上限");
                        ui.label(format!("{0} × {0} px", crate::loader::max_tex_side()));
                        ui.end_row();
                    });
                } else {
                    ui.label("尚未開啟圖片");
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(
                        "滾輪/←/→ 切換上下張　Ctrl+滾輪或捏合 縮放　拖曳 平移\n\
                         顯示方式：0 自動適應　3 填滿裁切　W 符合寬度　H 符合高度　1 原始大小\n\
                         雙擊 1:1↔返回　2 200%　R 旋轉　Space 播放/暫停　,/. 逐格\n\
                         S 升/降序　中鍵單擊/F 全螢幕　B 背景　N 取樣　F5 重新載入",
                    )
                    .size(11.5)
                    .weak(),
                );
            });
        self.show_info = open;
    }
}

impl eframe::App for ViewerApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "prefs", &self.prefs);
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // 把實際的 GPU 貼圖上限告訴解碼執行緒（egui 在第一幀後才知道真值）
        let side = ctx.input(|i| i.max_texture_side) as u32;
        if cfg!(debug_assertions) && side != crate::loader::max_tex_side() {
            eprintln!("[zoetrope] max_texture_side = {side}");
        }
        crate::loader::set_max_tex_side(side);
        self.process_events(ctx);
        self.handle_input(ctx);
        self.advance_animation(ctx);

        if !self.fullscreen {
            self.toolbar(ctx);
            self.status_bar(ctx);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(self.prefs.bg.color()))
            .show(ctx, |ui| {
                self.canvas(ui);
            });
        self.info_window(ctx);
    }
}

/// 一格滾輪（egui 約 50pt）在 egui 平滑化後會分散到連續數幀，
/// 用累加器聚合；回傳應翻頁數（正 = 下一張，滾輪向下時 delta 為負）。
const WHEEL_NOTCH: f32 = 45.0;

fn wheel_steps(accum: &mut f32, delta: f32) -> isize {
    *accum += delta;
    let mut steps: isize = 0;
    while *accum <= -WHEEL_NOTCH {
        *accum += WHEEL_NOTCH;
        steps += 1; // 向下 → 下一張
    }
    while *accum >= WHEEL_NOTCH {
        *accum -= WHEEL_NOTCH;
        steps -= 1; // 向上 → 上一張
    }
    steps
}

fn fmt_bytes(b: usize) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::wheel_steps;

    #[test]
    fn one_notch_down_is_next() {
        let mut acc = 0.0;
        assert_eq!(wheel_steps(&mut acc, -50.0), 1);
        assert!(acc.abs() < 6.0, "殘量應該很小：{acc}");
    }

    #[test]
    fn one_notch_up_is_prev() {
        let mut acc = 0.0;
        assert_eq!(wheel_steps(&mut acc, 50.0), -1);
    }

    #[test]
    fn smoothed_deltas_accumulate_to_one_step() {
        // egui 平滑滾動：一格拆成多幀的小增量，總和才算一格
        let mut acc = 0.0;
        assert_eq!(wheel_steps(&mut acc, -20.0), 0);
        assert_eq!(wheel_steps(&mut acc, -15.0), 0);
        assert_eq!(wheel_steps(&mut acc, -15.0), 1);
        assert_eq!(wheel_steps(&mut acc, -3.0), 0);
    }

    #[test]
    fn fast_spin_gives_multiple_steps() {
        let mut acc = 0.0;
        assert_eq!(wheel_steps(&mut acc, -150.0), 3);
        let mut acc = 0.0;
        assert_eq!(wheel_steps(&mut acc, 100.0), -2);
    }

    #[test]
    fn direction_flip_cancels_residual() {
        let mut acc = 0.0;
        assert_eq!(wheel_steps(&mut acc, -30.0), 0);
        assert_eq!(wheel_steps(&mut acc, 30.0), 0);
        assert!(acc.abs() < 0.001);
    }
}
