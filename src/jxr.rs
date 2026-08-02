//! JPEG XR（`.jxr` / `.wdp`，Microsoft HD Photo）解碼。
//!
//! Windows 的遊戲列（Win+Alt+PrtScn）在 HDR 模式下擷取的截圖就是這個格式，
//! 內含真正的浮點高動態範圍資料。
//!
//! 實作走 **Windows 內建的 WIC**：作業系統原生支援 JPEG XR，純 Rust 綁定、
//! 不需要任何 C 建置。（另一條路是 `jpegxr` crate，但它綁 Microsoft 的 C 版
//! jxrlib，在 mingw 下要修補 MSVC 專屬的 SAL 標註，還要額外安裝 libclang——
//! 對從原始碼建置的人門檻太高。）
//!
//! 非 Windows 平台不支援此格式（JPEG XR 幾乎只在 Windows 生態出現）。

use std::path::Path;

/// JXR 解碼結果：HDR 來源保留浮點，SDR 來源直接給 8-bit
pub enum JxrImage {
    Hdr(crate::hdr::HdrImage),
    Sdr(image::RgbaImage),
}

/// 副檔名判斷（WIC 也能靠內容辨識，但先用副檔名快速篩掉不相干的檔案）
pub const JXR_EXTS: &[&str] = &["jxr", "wdp", "hdp"];

pub fn is_jxr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| JXR_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// JPEG XR 的檔頭：`II` + 0xBC（TIFF 位元組序標記後接 JXR 版本）
pub fn is_jxr_header(head: &[u8]) -> bool {
    head.len() >= 4 && head[0] == b'I' && head[1] == b'I' && head[2] == 0xBC
}

#[cfg(windows)]
pub fn decode(path: &Path) -> Result<JxrImage, String> {
    win::decode(path)
}

#[cfg(not(windows))]
pub fn decode(_path: &Path) -> Result<JxrImage, String> {
    Err("JPEG XR 目前僅在 Windows 上支援（使用系統內建的 WIC 解碼器）".into())
}

#[cfg(windows)]
mod win {
    use super::JxrImage;
    use std::path::Path;
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat128bppRGBAFloat, GUID_WICPixelFormat32bppRGBA,
        IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
        WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };

    /// 解碼執行緒可能是任何一條，COM 必須逐執行緒初始化。
    /// 已初始化（含以不同模式初始化）都視為成功——我們只是要能用 WIC。
    fn ensure_com() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    fn err<E: std::fmt::Display>(what: &str, e: E) -> String {
        format!("JPEG XR {what}失敗：{e}")
    }

    pub fn decode(path: &Path) -> Result<JxrImage, String> {
        ensure_com();
        unsafe {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| err("初始化 WIC", e))?;

            // 用絕對路徑並經 to_string_lossy 明確轉換；相對路徑或非預期的
            // OsStr 轉換會讓 WIC 回報 ERROR_PATH_NOT_FOUND
            let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
            let wide = HSTRING::from(abs.to_string_lossy().as_ref());
            let decoder = factory
                .CreateDecoderFromFilename(
                    &wide,
                    None,
                    windows::Win32::Foundation::GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )
                .map_err(|e| format!("JPEG XR 開啟檔案失敗（{}）：{e}", abs.display()))?;

            let frame = decoder.GetFrame(0).map_err(|e| err("讀取影格", e))?;
            let (mut w, mut h) = (0u32, 0u32);
            frame
                .GetSize(&mut w, &mut h)
                .map_err(|e| err("取得尺寸", e))?;
            if w == 0 || h == 0 {
                return Err("JPEG XR 影像尺寸無效".into());
            }

            // 來源是浮點或高位元深度就走 HDR 路徑，保留動態範圍
            let src_fmt = frame.GetPixelFormat().map_err(|e| err("取得像素格式", e))?;
            let hdr_source = is_high_range(&src_fmt);

            let converter = factory
                .CreateFormatConverter()
                .map_err(|e| err("建立格式轉換器", e))?;
            let target = if hdr_source {
                GUID_WICPixelFormat128bppRGBAFloat
            } else {
                GUID_WICPixelFormat32bppRGBA
            };
            converter
                .Initialize(
                    &frame,
                    &target,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .map_err(|e| err("轉換像素格式", e))?;
            let source: windows::Win32::Graphics::Imaging::IWICBitmapSource =
                converter.cast().map_err(|e| err("取得像素來源", e))?;

            let (wu, hu) = (w as usize, h as usize);
            if hdr_source {
                let stride = wu * 16; // RGBA × f32
                let mut buf = vec![0f32; wu * hu * 4];
                let bytes = std::slice::from_raw_parts_mut(
                    buf.as_mut_ptr() as *mut u8,
                    std::mem::size_of_val(buf.as_slice()),
                );
                source
                    .CopyPixels(std::ptr::null(), stride as u32, bytes)
                    .map_err(|e| err("複製像素", e))?;
                let px = buf.iter().map(|v| crate::hdr::f32_to_f16(*v)).collect();
                Ok(JxrImage::Hdr(crate::hdr::HdrImage { size: [wu, hu], px }))
            } else {
                let stride = wu * 4;
                let mut buf = vec![0u8; wu * hu * 4];
                source
                    .CopyPixels(std::ptr::null(), stride as u32, &mut buf)
                    .map_err(|e| err("複製像素", e))?;
                image::RgbaImage::from_raw(w, h, buf)
                    .map(JxrImage::Sdr)
                    .ok_or_else(|| "JPEG XR 像素資料長度不符".to_string())
            }
        }
    }

    /// 判斷來源像素格式是否帶有超出 8-bit 的動態範圍
    fn is_high_range(fmt: &windows::core::GUID) -> bool {
        use windows::Win32::Graphics::Imaging::*;
        [
            GUID_WICPixelFormat128bppRGBAFloat,
            GUID_WICPixelFormat128bppRGBFloat,
            GUID_WICPixelFormat64bppRGBAHalf,
            GUID_WICPixelFormat64bppRGBHalf,
            GUID_WICPixelFormat96bppRGBFloat,
            GUID_WICPixelFormat32bppRGBE,
            GUID_WICPixelFormat64bppRGBA,
            GUID_WICPixelFormat48bppRGB,
        ]
        .contains(fmt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_detection() {
        assert!(is_jxr_path(Path::new("shot.jxr")));
        assert!(is_jxr_path(Path::new("SHOT.JXR")));
        assert!(is_jxr_path(Path::new("old.wdp")));
        assert!(!is_jxr_path(Path::new("photo.jpg")));
        assert!(!is_jxr_path(Path::new("noext")));
    }

    #[test]
    fn header_detection() {
        // JPEG XR：'I''I' 0xBC 0x01
        assert!(is_jxr_header(&[b'I', b'I', 0xBC, 0x01]));
        // 一般 TIFF（'I''I' 42 0）不該被誤判
        assert!(!is_jxr_header(&[b'I', b'I', 0x2A, 0x00]));
        // 太短
        assert!(!is_jxr_header(b"II"));
        assert!(!is_jxr_header(&[]));
    }
}
