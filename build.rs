fn main() {
    // Windows：把 assets/icon.ico 嵌進執行檔（檔案總管、工作列、檔案關聯用）。
    // 需要 windres（WinLibs/mingw 附帶）；失敗只警告不擋編譯。
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        if std::path::Path::new("assets/icon.ico").exists() {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            res.set("ProductName", "Zoetrope");
            res.set("FileDescription", "Zoetrope 走馬燈 — 極速看圖");
            if let Err(e) = res.compile() {
                println!("cargo:warning=icon resource embedding failed: {e}");
            }
        }
    }
}
