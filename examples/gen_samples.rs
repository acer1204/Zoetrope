//! 產生示範圖片：cargo run --release --example gen_samples [--big] [輸出資料夾]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let big = args.iter().any(|a| a == "--big");
    let dir = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "samples".to_owned());
    let files = zoetrope::samplegen::write_all(std::path::Path::new(&dir), big)
        .expect("樣本產生失敗");
    for f in files {
        println!("{}", f.display());
    }
}
