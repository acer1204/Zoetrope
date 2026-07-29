use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::types::{Decoded, CACHE_BUDGET_BYTES};

struct Entry {
    decoded: Arc<Decoded>,
    last_used: u64,
}

/// 以位元組數為預算的 LRU 解碼快取。
/// 目前顯示中的圖與其鄰居可設為 protected，不會被淘汰。
pub struct Cache {
    map: HashMap<PathBuf, Entry>,
    tick: u64,
    budget: usize,
    used: usize,
    protected: Vec<PathBuf>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::with_budget(CACHE_BUDGET_BYTES)
    }
}

impl Cache {
    pub fn with_budget(budget: usize) -> Self {
        Self {
            map: HashMap::new(),
            tick: 0,
            budget,
            used: 0,
            protected: Vec::new(),
        }
    }

    pub fn get(&mut self, path: &Path) -> Option<Arc<Decoded>> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(path).map(|e| {
            e.last_used = tick;
            e.decoded.clone()
        })
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.map.contains_key(path)
    }

    pub fn remove(&mut self, path: &Path) {
        if let Some(e) = self.map.remove(path) {
            self.used = self.used.saturating_sub(e.decoded.bytes);
        }
    }

    pub fn insert(&mut self, path: PathBuf, decoded: Arc<Decoded>) {
        self.remove(&path);
        self.tick += 1;
        self.used += decoded.bytes;
        self.map.insert(
            path.clone(),
            Entry {
                decoded,
                last_used: self.tick,
            },
        );
        self.evict(&path);
    }

    /// 設定不可淘汰的路徑（目前圖片與預載鄰居）
    pub fn set_protected(&mut self, paths: Vec<PathBuf>) {
        self.protected = paths;
    }

    fn evict(&mut self, just_inserted: &Path) {
        while self.used > self.budget && self.map.len() > 1 {
            let victim = self
                .map
                .iter()
                .filter(|(p, _)| {
                    p.as_path() != just_inserted && !self.protected.iter().any(|q| q == *p)
                })
                .min_by_key(|(_, e)| e.last_used)
                .map(|(p, _)| p.clone());
            match victim {
                Some(p) => self.remove(&p),
                None => break, // 只剩受保護的項目，允許暫時超出預算
            }
        }
    }

    /// (已用位元組, 項目數)
    pub fn stats(&self) -> (usize, usize) {
        (self.used, self.map.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FrameData, ImageMeta};
    use eframe::egui::ColorImage;
    use std::time::Duration;

    fn dummy(bytes_px: usize) -> Arc<Decoded> {
        let img = Arc::new(ColorImage::new([bytes_px, 1], eframe::egui::Color32::BLACK));
        let frames = vec![FrameData {
            image: img.clone(),
            delay: Duration::ZERO,
        }];
        let bytes = Decoded::compute_bytes(&frames, &[]);
        Arc::new(Decoded {
            meta: ImageMeta {
                path: PathBuf::new(),
                orig_size: [bytes_px as u32, 1],
                file_size: 0,
                format: "TEST".into(),
                animated: false,
                has_alpha: false,
            },
            frames,
            mips: vec![],
            complete: true,
            truncated: false,
            bytes,
        })
    }

    #[test]
    fn lru_evicts_oldest_unprotected() {
        let mut c = Cache::with_budget(100 * 4);
        c.insert(PathBuf::from("a"), dummy(40));
        c.insert(PathBuf::from("b"), dummy(40));
        assert!(c.contains(Path::new("a")) && c.contains(Path::new("b")));
        let _ = c.get(Path::new("a")); // a 變成較新
        c.insert(PathBuf::from("c"), dummy(40)); // 超出預算 → 淘汰 b
        assert!(c.contains(Path::new("a")));
        assert!(!c.contains(Path::new("b")));
        assert!(c.contains(Path::new("c")));
    }

    #[test]
    fn protected_survives() {
        let mut c = Cache::with_budget(50 * 4);
        c.insert(PathBuf::from("a"), dummy(40));
        c.set_protected(vec![PathBuf::from("a")]);
        c.insert(PathBuf::from("b"), dummy(40));
        assert!(c.contains(Path::new("a")), "受保護項目不應被淘汰");
    }
}
