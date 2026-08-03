//! HDR 顯示診斷：比較各條色調映射曲線在同一張檔案上的輸出分佈。
//!
//! 懷疑某張 HDR 圖顯示不正確時，跑這個把數據印出來，與其他看圖軟體
//! （例如 Windows 相簿）的結果對照，就能判斷是不是設定不對，
//! 不必靠眼睛猜。只讀取像素統計，不顯示也不儲存影像內容。
//!
//! ```text
//! cargo run --release --example hdr_compare -- 你的檔案.jxr
//! ```

use zoetrope::hdr::{self, HdrImage, ToneOp};

fn pct_u8(vals: &mut [u8], q: usize) -> u8 {
    vals[(vals.len().saturating_sub(1)).min(vals.len() * q / 100)]
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("用法：hdr_compare <檔案路徑>");
        std::process::exit(2);
    };
    let p = std::path::Path::new(&path);

    let img: HdrImage = if zoetrope::jxr::is_jxr_path(p) {
        match zoetrope::jxr::decode(p).expect("JXR 解碼") {
            zoetrope::jxr::JxrImage::Hdr(h) => h,
            zoetrope::jxr::JxrImage::Sdr(_) => {
                println!("這是 SDR 來源，沒有色調映射問題");
                return;
            }
        }
    } else {
        let d = image::open(p).expect("解碼");
        hdr::from_dynamic(&d).expect("這個檔案沒有浮點像素資料")
    };

    let ev = img.kind.default_exposure_ev();
    let op = img.kind.default_tone_op();
    println!(
        "檔案      {}",
        p.file_name().unwrap_or_default().to_string_lossy()
    );
    println!("尺寸      {}×{}", img.size[0], img.size[1]);
    println!("來源類型  {:?}", img.kind);
    println!("預設設定  {:+.2} EV · {}", ev, op.name());

    let mut src: Vec<f32> = img
        .px
        .chunks_exact(4)
        .step_by(37)
        .map(|c| hdr::f16_to_f32(c[0]))
        .collect();
    src.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let f_at = |q: usize| src[(src.len().saturating_sub(1)).min(src.len() * q / 100)];
    let over1 = src.iter().filter(|v| **v > 1.0).count();
    println!(
        "\n來源浮點 R  p01={:.4}  p25={:.4}  p50={:.4}  p90={:.4}  p99={:.4}  max={:.4}",
        f_at(1),
        f_at(25),
        f_at(50),
        f_at(90),
        f_at(99),
        src[src.len() - 1]
    );
    println!(
        "超過 1.0 的像素：{:.1}%",
        100.0 * over1 as f64 / src.len() as f64
    );

    // 飽和度：(max-min)/max，在線性空間量。逐通道壓縮會把它拉低——
    // 這正是「膚色偏白」在數字上的樣子。
    let to_linear = |v: u8| {
        let x = v as f32 / 255.0;
        if x <= 0.040_45 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };

    println!("\n輸出 R 通道分佈（sat = 亮部平均飽和度，ms = 映射耗時）：");
    println!(
        "{:<22} {:>5} {:>5} {:>5} {:>5} {:>5} {:>6} {:>7}",
        "設定", "p01", "p25", "p50", "p90", "p99", "sat", "ms"
    );
    let show = |label: String, ev: f32, op: ToneOp| {
        let lut = hdr::ToneLut::build(ev, op);
        let t = std::time::Instant::now();
        let out = hdr::tonemap(&img, &lut);
        let ms = t.elapsed().as_secs_f64() * 1000.0;

        let mut v: Vec<u8> = out.pixels.iter().step_by(37).map(|c| c.r()).collect();
        v.sort_unstable();

        let mut sat = 0.0f64;
        let mut n = 0u64;
        for c in out.pixels.iter().step_by(37) {
            let l = [to_linear(c.r()), to_linear(c.g()), to_linear(c.b())];
            let max = l[0].max(l[1]).max(l[2]);
            // 只統計亮部：暗部的飽和度受量化雜訊影響太大
            if max > 0.2 {
                sat += ((max - l[0].min(l[1]).min(l[2])) / max) as f64;
                n += 1;
            }
        }
        println!(
            "{:<22} {:>5} {:>5} {:>5} {:>5} {:>5} {:>6.3} {:>7.1}",
            label,
            pct_u8(&mut v, 1),
            pct_u8(&mut v, 25),
            pct_u8(&mut v, 50),
            pct_u8(&mut v, 90),
            pct_u8(&mut v, 99),
            if n > 0 { sat / n as f64 } else { 0.0 },
            ms
        );
    };

    show(format!("預設（{:+.1}EV {}）", ev, op.name()), ev, op);
    for o in [ToneOp::Clip, ToneOp::Aces, ToneOp::Reinhard] {
        show(format!("{:+.1}EV {}", ev, o.name()), ev, o);
    }
    for o in [ToneOp::Clip, ToneOp::Aces, ToneOp::Reinhard] {
        show(format!("0.0EV {}", o.name()), 0.0, o);
    }

    println!(
        "\n說明：顯示參考（scRGB）的來源要把「紙白」拉回 SDR 白，因此預設帶約 −1.5 EV；\n\
         若直接用 0 EV，等於把 scRGB 1.0（僅 80 nits）當成白，整張會明顯偏亮。"
    );
}
