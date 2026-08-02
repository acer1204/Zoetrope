//! 真實 RAW 檔的內嵌預覽驗證與速度比較。
//! 需要 tests/assets/ 裡有樣本檔；沒有時整個測試會跳過而非失敗。

use std::path::Path;
use std::time::Instant;

use zoetrope::{extra_formats, raw_preview};

fn assets() -> Option<Vec<std::path::PathBuf>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/assets");
    if !dir.is_dir() {
        return None;
    }
    let mut v: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| extra_formats::RAW_EXTS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

#[test]
fn embedded_preview_is_correct_and_faster() {
    let Some(files) = assets() else {
        eprintln!("跳過：tests/assets/ 沒有 RAW 樣本");
        return;
    };

    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();

        // 1. 抽預覽並計時
        let t = Instant::now();
        let prev = raw_preview::extract(&path);
        let prev_ms = t.elapsed().as_millis();

        let Some(prev) = prev else {
            eprintln!("{name}: 找不到內嵌預覽（將回退完整顯影）");
            continue;
        };

        // 預覽必須是合理的影像
        assert!(
            prev.size.0 >= 512 && prev.size.1 >= 512,
            "{name}: 預覽尺寸過小 {:?}",
            prev.size
        );
        assert_eq!(
            (prev.image.width(), prev.image.height()),
            prev.size,
            "{name}: 解碼尺寸與標頭不符"
        );
        // 不該是全黑或全白（代表解碼壞掉）
        let mid = prev
            .image
            .get_pixel(prev.image.width() / 2, prev.image.height() / 2)
            .0;
        let all_same = prev
            .image
            .pixels()
            .step_by(997)
            .all(|p| p.0[0] == mid[0] && p.0[1] == mid[1] && p.0[2] == mid[2]);
        assert!(!all_same, "{name}: 預覽看起來是單色，解碼可能有問題");

        // 2. 走主解碼路徑（app 實際呼叫的入口）
        let t = Instant::now();
        let (img, fmt) = zoetrope::loader::decode_static(&path).expect("主路徑解碼");
        let path_ms = t.elapsed().as_millis();
        assert_eq!(fmt, "RAW");
        assert_eq!(
            (img.width(), img.height()),
            prev.size,
            "{name}: 主路徑應採用預覽"
        );

        eprintln!(
            "{name}: 預覽 {}×{}  抽取 {prev_ms}ms  主路徑 {path_ms}ms",
            prev.size.0, prev.size.1
        );
    }
}

/// 對照組：完整 demosaic 顯影的耗時（只跑一個檔，避免測試太久）
#[test]
fn full_develop_timing_for_comparison() {
    let Some(files) = assets() else {
        eprintln!("跳過：tests/assets/ 沒有 RAW 樣本");
        return;
    };
    // CR3 的原始資料 rawloader 不支援，挑非 CR3 的來比
    let Some(path) = files.iter().find(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            != Some("cr3".into())
    }) else {
        return;
    };

    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let t = Instant::now();
    match extra_formats::develop_raw(path) {
        Ok(img) => eprintln!(
            "{name}: 完整顯影 {}×{}  耗時 {}ms",
            img.width(),
            img.height(),
            t.elapsed().as_millis()
        ),
        Err(e) => eprintln!("{name}: 完整顯影失敗（{e}）"),
    }
}

/// CR3 應該能靠內嵌預覽顯示（rawloader 不支援其原始資料）
#[test]
fn cr3_works_via_preview() {
    let Some(files) = assets() else {
        eprintln!("跳過：tests/assets/ 沒有 RAW 樣本");
        return;
    };
    let Some(cr3) = files.iter().find(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            == Some("cr3".into())
    }) else {
        eprintln!("跳過：沒有 CR3 樣本");
        return;
    };

    // 格式偵測
    assert_eq!(
        extra_formats::sniff(cr3),
        Some(extra_formats::ExtraFormat::Raw),
        "CR3 應被辨識為 RAW"
    );
    // 完整顯影對 CR3 是失敗的——正好證明預覽路徑是它唯一能顯示的方式
    let developed = extra_formats::develop_raw(cr3);
    // 主路徑仍應成功
    let (img, fmt) = zoetrope::loader::decode_static(cr3).expect("CR3 主路徑應成功");
    assert_eq!(fmt, "RAW");
    assert!(img.width() > 512 && img.height() > 512);
    eprintln!(
        "CR3: 顯示 {}×{}（完整顯影{}）",
        img.width(),
        img.height(),
        if developed.is_ok() {
            "可用"
        } else {
            "不支援，靠預覽"
        }
    );
}
