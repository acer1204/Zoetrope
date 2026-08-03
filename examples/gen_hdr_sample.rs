// 一次性：產生一張視覺上有意義的 HDR 測試圖（日落，含超過 1.0 的高光）
fn main() {
    let (w, h) = (1200u32, 800u32);
    let img = image::Rgb32FImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let horizon = 0.62;
        // 天空：接近地平線變暖
        let t = (fy / horizon).clamp(0.0, 1.0);
        let e = t * t;
        let mut c = [0.02 + 0.30 * e, 0.03 + 0.14 * e, 0.10 + 0.02 * e];
        // 太陽：亮度到 60（遠超過螢幕能表達的 1.0）
        let d = ((fx - 0.70) * 1.5).hypot(fy - 0.44);
        if d < 0.30 {
            let glow = (1.0 - d / 0.30).powf(3.0);
            c[0] += 60.0 * glow;
            c[1] += 38.0 * glow;
            c[2] += 12.0 * glow;
        }
        // 山：暗部細節（曝光拉高才看得見）
        let ridge = horizon + 0.05 + 0.06 * (fx * 9.0).sin();
        if fy > ridge {
            let depth = (fy - ridge) * 2.0;
            c = [
                0.004 + 0.02 * depth,
                0.006 + 0.02 * depth,
                0.012 + 0.02 * depth,
            ];
        }
        image::Rgb(c)
    });
    let out = std::env::args().nth(1).expect("需要輸出路徑");
    image::DynamicImage::ImageRgb32F(img)
        .save_with_format(&out, image::ImageFormat::OpenExr)
        .expect("寫入 EXR");
    println!("{out}");
}
