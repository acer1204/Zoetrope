// 一次性比對：用專案的解碼路徑處理一張真實 JXR，輸出各百分位數，
// 與 Windows 相簿的結果對照（不顯示、不儲存影像內容）
fn main() {
    let path = std::env::args().nth(1).expect("需要 .jxr 路徑");
    let p = std::path::Path::new(&path);

    let img = match zoetrope::jxr::decode(p).expect("解碼") {
        zoetrope::jxr::JxrImage::Hdr(h) => h,
        zoetrope::jxr::JxrImage::Sdr(_) => {
            println!("這是 SDR 來源，不需比對");
            return;
        }
    };
    println!(
        "尺寸 {}x{}  來源類型 {:?}",
        img.size[0], img.size[1], img.kind
    );

    // 來源浮點分佈
    let mut src: Vec<f32> = img
        .px
        .chunks_exact(4)
        .step_by(37)
        .map(|c| zoetrope::hdr::f16_to_f32(c[0]))
        .collect();
    src.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pf = |v: &Vec<f32>, q: usize| v[(v.len() - 1).min(v.len() * q / 100)];
    println!(
        "來源 float R：p01={:.4} p50={:.4} p90={:.4} max={:.4}",
        pf(&src, 1),
        pf(&src, 50),
        pf(&src, 90),
        src[src.len() - 1]
    );

    // 套用預設曲線後的輸出分佈
    let lut = zoetrope::hdr::ToneLut::build(0.0, img.kind.default_tone_op());
    let out = zoetrope::hdr::tonemap(&img, &lut);
    let mut got: Vec<u8> = out.pixels.iter().step_by(37).map(|p| p.r()).collect();
    got.sort_unstable();
    let pu = |v: &Vec<u8>, q: usize| v[(v.len() - 1).min(v.len() * q / 100)];
    println!(
        "Zoetrope 輸出 R：p01={} p50={} p90={} max={}  （曲線 {:?}）",
        pu(&got, 1),
        pu(&got, 50),
        pu(&got, 90),
        got[got.len() - 1],
        img.kind.default_tone_op()
    );
}
