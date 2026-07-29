# 真實樣本檔測試資料夾

把任何 `.jxl` / `.avif` / `.heic` / `.heif` / `.cr2` / `.nef` / `.arw` / `.dng` 等
樣本檔丟進這個資料夾，`tests/extra_formats.rs` 的 `real_world_samples_if_present`
測試就會自動抓來驗證（解碼成功、尺寸合理、非全透明）。資料夾空的時候測試會跳過而不失敗。

樣本檔本身不納入版控（見 `.gitignore`），避免專案體積膨脹與第三方測試檔的授權問題。

驗證過的公開來源：

- **JPEG XL**：<https://github.com/libjxl/testdata>
- **HEIC/HEIF**：<https://github.com/nokiatech/heif_conformance>
- **相機 RAW**：<https://raw.pixls.us/>（依 廠牌/機型 分類瀏覽）
- **AVIF**：不需下載，`tests/extra_formats.rs` 會用 `ravif` 現場產生
