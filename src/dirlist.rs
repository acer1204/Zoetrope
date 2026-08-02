use std::cmp::Ordering;
use std::path::Path;

use crate::types::FileEntry;

/// image crate 直接支援的副檔名
pub const BASE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "webp", "bmp", "dib", "ico", "tif", "tiff",
    "tga", "qoi", "hdr", "exr", "pnm", "pbm", "pgm", "ppm", "dds", "ff",
];

/// 全部支援的副檔名（含 JPEG XL / AVIF / HEIC / RAW），
/// 用於資料夾掃描、拖放與開檔對話框過濾
pub static EXTS: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
    let mut v: Vec<&'static str> = BASE_EXTS.to_vec();
    v.extend_from_slice(crate::extra_formats::MODERN_EXTS);
    v.extend_from_slice(crate::extra_formats::RAW_EXTS);
    v.extend_from_slice(crate::jxr::JXR_EXTS);
    v
});

pub fn is_image_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            EXTS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// 列出資料夾內所有圖片（不排序；排序由 UI 依設定執行）。
/// 只讀目錄項目、不開檔；大小與修改時間直接取自目錄資料，萬張等級毫秒完成。
pub fn scan_dir(dir: &Path) -> Vec<FileEntry> {
    match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if !is_image_path(&path) {
                    return None;
                }
                let md = e.metadata().ok()?;
                if !md.is_file() {
                    return None;
                }
                Some(FileEntry {
                    path,
                    modified: md.modified().ok(),
                    size: md.len(),
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 排序依據（含升降序由呼叫端指定）
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SortKey {
    /// 檔案名稱（自然排序：img2 < img10）
    Name,
    /// 最後修改日期
    Modified,
    /// 檔案大小
    Size,
    /// 檔案類型（副檔名）
    Type,
}

impl SortKey {
    pub fn name(self) -> &'static str {
        match self {
            SortKey::Name => "檔案名稱",
            SortKey::Modified => "修改日期",
            SortKey::Size => "檔案大小",
            SortKey::Type => "檔案類型",
        }
    }
}

pub fn sort_entries(entries: &mut [FileEntry], key: SortKey, ascending: bool) {
    let ext_of = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default()
    };
    entries.sort_by(|a, b| {
        let ord = match key {
            SortKey::Name => natural_path_cmp(&a.path, &b.path),
            SortKey::Modified => a
                .modified
                .cmp(&b.modified)
                .then_with(|| natural_path_cmp(&a.path, &b.path)),
            SortKey::Size => a
                .size
                .cmp(&b.size)
                .then_with(|| natural_path_cmp(&a.path, &b.path)),
            SortKey::Type => ext_of(&a.path)
                .cmp(&ext_of(&b.path))
                .then_with(|| natural_path_cmp(&a.path, &b.path)),
        };
        if ascending {
            ord
        } else {
            ord.reverse()
        }
    });
}

pub fn natural_path_cmp(a: &Path, b: &Path) -> Ordering {
    let an = a.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let bn = b.file_name().and_then(|s| s.to_str()).unwrap_or("");
    natural_cmp(an, bn).then_with(|| a.cmp(b))
}

/// 不分大小寫的自然排序：數字段落以數值比較（"img2" < "img10"）。
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let (ca, cb) = (a[i], b[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            // 取出兩邊完整的數字段
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            // 跳過前導零後比較：位數多者大，位數相同逐字比較
            let da = trim_zeros(&a[si..i]);
            let db = trim_zeros(&b[sj..j]);
            let ord = da
                .len()
                .cmp(&db.len())
                .then_with(|| da.cmp(db))
                // 數值相同時，前導零較少者排前（"1" < "01"）
                .then_with(|| (i - si).cmp(&(j - sj)));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            let la = ca.to_lowercase().next().unwrap_or(ca);
            let lb = cb.to_lowercase().next().unwrap_or(cb);
            let ord = la.cmp(&lb).then_with(|| ca.cmp(&cb));
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        }
    }
    a.len().cmp(&b.len()).then_with(|| (i).cmp(&j))
}

fn trim_zeros(s: &[char]) -> &[char] {
    let mut k = 0;
    while k + 1 < s.len() && s[k] == '0' {
        k += 1;
    }
    &s[k..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_order() {
        let mut v = vec!["img10.png", "img2.png", "img1.png", "img100.png"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["img1.png", "img2.png", "img10.png", "img100.png"]);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(natural_cmp("Apple.png", "apple.png"), Ordering::Less); // 同名時大寫排前，穩定即可
        let mut v = vec!["b.png", "A.png", "c.png"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["A.png", "b.png", "c.png"]);
    }

    #[test]
    fn leading_zeros() {
        let mut v = vec!["img01.png", "img1.png", "img002.png"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["img1.png", "img01.png", "img002.png"]);
    }

    #[test]
    fn mixed_segments() {
        assert_eq!(natural_cmp("a2b10", "a2b9"), Ordering::Greater);
        assert_eq!(natural_cmp("a2b", "a10b"), Ordering::Less);
    }

    #[test]
    fn ext_filter() {
        assert!(is_image_path(Path::new("x/y/photo.JPG")));
        assert!(is_image_path(Path::new("動畫.webp")));
        assert!(!is_image_path(Path::new("doc.txt")));
        assert!(!is_image_path(Path::new("noext")));
    }

    #[test]
    fn ext_filter_includes_modern_and_raw() {
        for name in [
            "pic.jxl",
            "pic.avif",
            "IMG_1234.HEIC",
            "IMG_1234.heif",
            "shot.CR2",
            "shot.nef",
            "shot.arw",
            "shot.dng",
        ] {
            assert!(is_image_path(Path::new(name)), "應辨識 {name}");
        }
        assert!(!is_image_path(Path::new("clip.mp4")));
    }

    fn entry(name: &str, secs: u64, size: u64) -> FileEntry {
        FileEntry {
            path: std::path::PathBuf::from(name),
            modified: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
            size,
        }
    }

    fn names(v: &[FileEntry]) -> Vec<&str> {
        v.iter().map(|e| e.path.to_str().unwrap()).collect()
    }

    #[test]
    fn sort_by_each_key() {
        let make = || {
            vec![
                entry("b10.png", 200, 5),
                entry("b2.gif", 300, 1),
                entry("a.webp", 100, 9),
            ]
        };

        let mut v = make();
        sort_entries(&mut v, SortKey::Name, true);
        assert_eq!(names(&v), ["a.webp", "b2.gif", "b10.png"]);
        sort_entries(&mut v, SortKey::Name, false);
        assert_eq!(names(&v), ["b10.png", "b2.gif", "a.webp"]);

        let mut v = make();
        sort_entries(&mut v, SortKey::Modified, true);
        assert_eq!(names(&v), ["a.webp", "b10.png", "b2.gif"]);
        sort_entries(&mut v, SortKey::Modified, false);
        assert_eq!(names(&v), ["b2.gif", "b10.png", "a.webp"]);

        let mut v = make();
        sort_entries(&mut v, SortKey::Size, true);
        assert_eq!(names(&v), ["b2.gif", "b10.png", "a.webp"]);

        let mut v = make();
        sort_entries(&mut v, SortKey::Type, true);
        assert_eq!(names(&v), ["b2.gif", "b10.png", "a.webp"]); // gif < png < webp
    }

    #[test]
    fn sort_modified_none_first() {
        let mut v = vec![entry("new.png", 100, 1), {
            let mut e = entry("unknown.png", 0, 1);
            e.modified = None;
            e
        }];
        sort_entries(&mut v, SortKey::Modified, true);
        assert_eq!(names(&v), ["unknown.png", "new.png"]);
    }
}
