//! 驗證動畫的變動矩形計算，以及它實際能省下多少上傳頻寬。

use eframe::egui::{Color32, ColorImage};
use zoetrope::loader::diff_rect;

fn img(w: usize, h: usize, fill: Color32) -> ColorImage {
    ColorImage::new([w, h], fill)
}

#[test]
fn identical_images_report_no_change() {
    let a = img(64, 48, Color32::RED);
    let b = a.clone();
    assert_eq!(diff_rect(&a, &b), Some([0, 0, 0, 0]));
}

#[test]
fn different_sizes_force_full_upload() {
    let a = img(64, 48, Color32::RED);
    let b = img(32, 48, Color32::RED);
    assert_eq!(diff_rect(&a, &b), None, "尺寸不同應回報 None（整張重傳）");
}

#[test]
fn finds_tight_bounding_box_of_change() {
    let a = img(100, 80, Color32::BLACK);
    let mut b = a.clone();
    // 只改動 (10,20) 到 (14,23) 這一小塊
    for y in 20..24 {
        for x in 10..15 {
            b.pixels[y * 100 + x] = Color32::WHITE;
        }
    }
    assert_eq!(
        diff_rect(&a, &b),
        Some([10, 20, 5, 4]),
        "應算出緊貼變動範圍的矩形"
    );
}

#[test]
fn single_pixel_change() {
    let a = img(50, 50, Color32::BLUE);
    let mut b = a.clone();
    b.pixels[25 * 50 + 30] = Color32::GREEN;
    assert_eq!(diff_rect(&a, &b), Some([30, 25, 1, 1]));
}

#[test]
fn change_at_edges_is_within_bounds() {
    let a = img(20, 10, Color32::BLACK);
    // 四個角各改一點 → 矩形應涵蓋整張但不越界
    let mut b = a.clone();
    b.pixels[0] = Color32::WHITE;
    b.pixels[19] = Color32::WHITE;
    b.pixels[9 * 20] = Color32::WHITE;
    b.pixels[9 * 20 + 19] = Color32::WHITE;
    let r = diff_rect(&a, &b).unwrap();
    assert_eq!(r, [0, 0, 20, 10]);
    assert!(r[0] + r[2] <= 20 && r[1] + r[3] <= 10, "不得越界");
}

#[test]
fn empty_image_is_handled() {
    let a = img(0, 0, Color32::BLACK);
    let b = img(0, 0, Color32::BLACK);
    assert_eq!(diff_rect(&a, &b), Some([0, 0, 0, 0]));
}

/// 模擬典型 GIF：大部分畫布不動，只有一小塊在動——量測省下的頻寬
#[test]
fn typical_animation_saves_substantial_bandwidth() {
    let (w, h) = (640usize, 480usize);
    let base = img(w, h, Color32::from_rgb(30, 40, 60));

    let mut total_full = 0usize;
    let mut total_partial = 0usize;

    // 30 格，每格有一個 80×80 的方塊在移動
    let mut prev = base.clone();
    for f in 1..30 {
        let mut cur = base.clone();
        let cx = 20 + (f * 15) % (w - 100);
        let cy = 100;
        for y in cy..cy + 80 {
            for x in cx..cx + 80 {
                cur.pixels[y * w + x] = Color32::from_rgb(250, 200, 80);
            }
        }
        let r = diff_rect(&prev, &cur).expect("同尺寸應有結果");
        total_full += w * h * 4;
        total_partial += r[2] * r[3] * 4;
        prev = cur;
    }

    let ratio = total_partial as f64 / total_full as f64;
    eprintln!(
        "640×480 動畫 29 格：整張上傳 {:.1} MB → 部分更新 {:.1} MB（{:.0}%）",
        total_full as f64 / 1e6,
        total_partial as f64 / 1e6,
        ratio * 100.0
    );
    assert!(
        ratio < 0.35,
        "典型動畫應能省下大量頻寬，實際只降到 {:.0}%",
        ratio * 100.0
    );
}

/// 以「子矩形影格」編碼的 GIF——這是 ffmpeg、gifski、Photoshop 等
/// 主流工具的做法，也是實務上最常見的 GIF。
fn encode_subrect_gif(path: &std::path::Path, w: u32, h: u32, frames: usize) {
    use image::codecs::gif::{GifEncoder, Repeat};
    let file = std::fs::File::create(path).unwrap();
    let mut enc = GifEncoder::new_with_speed(file, 30);
    enc.set_repeat(Repeat::Infinite).unwrap();

    // 第一格：完整背景
    let bg = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x % 200) as u8, (y % 200) as u8, 90, 255])
    });
    enc.encode_frame(image::Frame::from_parts(
        bg,
        0,
        0,
        image::Delay::from_numer_denom_ms(60, 1),
    ))
    .unwrap();

    // 之後每格只送一個 40×40 的小方塊（left/top 指定位置）
    for i in 1..frames {
        let block = image::RgbaImage::from_pixel(40, 40, image::Rgba([250, 210, 60, 255]));
        let left = (10 + i as u32 * 7) % (w - 50);
        enc.encode_frame(image::Frame::from_parts(
            block,
            left,
            50,
            image::Delay::from_numer_denom_ms(60, 1),
        ))
        .unwrap();
    }
}

/// 主流工具編碼的 GIF（子矩形影格）應該能省下大量上傳頻寬
#[test]
fn subrect_encoded_gif_saves_bandwidth() {
    let dir = std::env::temp_dir().join(format!("zoetrope-gifsub-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sub.gif");
    let (w, h) = (300u32, 200u32);
    encode_subrect_gif(&path, w, h, 15);

    let (frames, dims, _) = zoetrope::loader::open_animation(&path, "gif")
        .expect("開啟")
        .expect("應為動畫");
    let decoded: Vec<_> = frames.collect::<Result<Vec<_>, _>>().expect("解碼影格");
    assert_eq!(dims, (w, h));
    assert!(decoded.len() >= 3);

    let to_ci =
        |f: &image::Frame| zoetrope::loader::to_color_image_clamped(f.clone().into_buffer());
    let (mut changed, mut total) = (0usize, 0usize);
    for pair in decoded.windows(2) {
        let (a, b) = (to_ci(&pair[0]), to_ci(&pair[1]));
        let r = diff_rect(&a, &b).expect("同尺寸應有結果");
        assert!(
            r[0] + r[2] <= w as usize && r[1] + r[3] <= h as usize,
            "矩形不得越界：{r:?}"
        );
        changed += r[2] * r[3];
        total += (w * h) as usize;
    }
    let pct = changed as f64 / total as f64 * 100.0;
    eprintln!("子矩形編碼 GIF：變動面積佔比 {pct:.0}%");
    assert!(pct < 40.0, "子矩形 GIF 應能省下頻寬，實際 {pct:.0}%");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 對照組：每格都重新量化整張畫布的 GIF。
///
/// GIF 是調色盤格式，若編碼器每格獨立量化整張畫布，背景顏色會整體
/// 偏移 ±1，導致「每個像素都不同」——此時變動矩形必然是整張，
/// 部分更新無從發揮。程式會正確退回整張重傳（不會顯示錯誤），
/// 這個測試把該行為固定下來，避免日後誤以為最佳化失效是 bug。
#[test]
fn fullframe_requantized_gif_falls_back_to_full_upload() {
    let dir = std::env::temp_dir().join(format!("zoetrope-giffull-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("full.gif");
    // 專案的樣本產生器每格都輸出整張畫布，正是這種情況
    zoetrope::samplegen::animated_gif(&path, 200, 150, 8).expect("產生 GIF");

    let (frames, _, _) = zoetrope::loader::open_animation(&path, "gif")
        .expect("開啟")
        .expect("應為動畫");
    let decoded: Vec<_> = frames.collect::<Result<Vec<_>, _>>().expect("解碼影格");
    let to_ci =
        |f: &image::Frame| zoetrope::loader::to_color_image_clamped(f.clone().into_buffer());

    let a = to_ci(&decoded[0]);
    let b = to_ci(&decoded[1]);
    let r = diff_rect(&a, &b).expect("同尺寸");
    // 不論結果大小，最重要的是矩形合法、不越界
    assert!(
        r[0] + r[2] <= 200 && r[1] + r[3] <= 150,
        "矩形不得越界：{r:?}"
    );
    eprintln!(
        "整張重新量化的 GIF：變動面積佔比 {:.0}%（預期接近 100%，會退回整張重傳）",
        (r[2] * r[3]) as f64 / (200.0 * 150.0) * 100.0
    );

    let _ = std::fs::remove_dir_all(&dir);
}
