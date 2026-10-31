//! 原生整屏截图命令(`capture_screen`,capture 拥有)。
//!
//! 实现路线(DESIGN §2):Win32 GDI `BitBlt` 捕获**虚拟屏幕**整屏
//! (`SM_XVIRTUALSCREEN`/`SM_YVIRTUALSCREEN`/`SM_CXVIRTUALSCREEN`/
//! `SM_CYVIRTUALSCREEN`,支持多显示器与负坐标),`GetDIBits` 取 32 位
//! BGRA 像素,逐行还原为 RGBA 后由 `png` crate 编码,最后 base64 成
//! data-URI 返回 `{ dataUrl, width, height }`。
//!
//! 约束:
//! - 不引入 `windows` crate,只用裸 `extern "system"` FFI(Win32 稳定 ABI);
//! - 非 Windows 平台返回明确错误(命令名仍存在,便于前端统一降级);
//! - 捕获资源一律 RAII 释放:`ReleaseDC` / `DeleteDC` / `DeleteObject`,
//!   任何失败路径都不泄漏 GDI 句柄;
//! - 模块本身不依赖 Tauri,`#[tauri::command]` 包装只在 `desktop` feature 下编译,
//!   因此默认特性 `cargo test` 也能测试 PNG/BGRA 纯逻辑。

use crate::shared::error::CoreErrorCode;
use crate::shared::AppError;

/// 截图返回:虚拟屏幕物理像素 PNG data-URI 与像素尺寸(IPC 为 camelCase)。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureScreenResult {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
}

fn screenshot_error(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::Internal, "capture_screen").with_detail("reason", reason.into())
}

/// 捕获虚拟屏幕整屏并编码为 PNG data-URI。
#[cfg(windows)]
pub fn capture_virtual_screen() -> Result<CaptureScreenResult, AppError> {
    let (origin_x, origin_y, width, height) = unsafe {
        (
            win32::GetSystemMetrics(win32::SM_XVIRTUALSCREEN),
            win32::GetSystemMetrics(win32::SM_YVIRTUALSCREEN),
            win32::GetSystemMetrics(win32::SM_CXVIRTUALSCREEN),
            win32::GetSystemMetrics(win32::SM_CYVIRTUALSCREEN),
        )
    };
    if width <= 0 || height <= 0 {
        return Err(screenshot_error("virtual_screen_empty"));
    }

    let mut capture = GdiCapture::new(width, height)?;
    capture.copy_from_screen(origin_x, origin_y, width, height)?;
    let (bgra, stride) = capture.read_bgra(width as u32, height as u32)?;

    let rgba = bgra_to_rgba(&bgra, width as u32, height as u32, stride);
    let png = encode_png(&rgba, width as u32, height as u32)?;
    let data_url = format!(
        "data:image/png;base64,{}",
        crate::shared::dto::base64_encode(&png)
    );
    Ok(CaptureScreenResult {
        data_url,
        width: width as u32,
        height: height as u32,
    })
}

#[cfg(not(windows))]
pub fn capture_virtual_screen() -> Result<CaptureScreenResult, AppError> {
    Err(screenshot_error("capture_unsupported_platform"))
}

/// 前端命令入口(`client.callFlat("capture_screen")`,无 request 形参)。
#[cfg(feature = "desktop")]
#[tauri::command]
pub fn capture_screen() -> Result<CaptureScreenResult, AppError> {
    capture_virtual_screen()
}

/// 32 位 BGRA(GDI 自下而上/自上而下由 biHeight 决定)按行还原为 RGBA。
///
/// - `stride` 为一行字节数(32bpp 下 `width*4` 补齐到 4 字节倍数);
/// - GDI 的 32bpp alpha 通道不可靠(BitBlt 后通常为 0),输出恒置 255,
///   保证 PNG 不透明,前端覆盖层不会透出桌面。
pub fn bgra_to_rgba(bgra: &[u8], width: u32, height: u32, stride: usize) -> Vec<u8> {
    let row_bytes = width as usize * 4;
    let mut rgba = vec![0u8; row_bytes * height as usize];
    for y in 0..height as usize {
        let src_start = y * stride;
        let Some(src_row) = bgra.get(src_start..src_start + row_bytes) else {
            break;
        };
        let dst_row = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
        let (src_pixels, _) = src_row.as_chunks::<4>();
        let (dst_pixels, _) = dst_row.as_chunks_mut::<4>();
        for (src_px, dst_px) in src_pixels.iter().zip(dst_pixels.iter_mut()) {
            dst_px[0] = src_px[2]; // R ← B
            dst_px[1] = src_px[1]; // G
            dst_px[2] = src_px[0]; // B ← R
            dst_px[3] = 0xFF;
        }
    }
    rgba
}

/// RGBA8 像素编码为 PNG(不压缩元数据;默认 zlib 压缩级别足够截图使用)。
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, AppError> {
    if width == 0 || height == 0 {
        return Err(screenshot_error("empty_image"));
    }
    let expected = width as usize * height as usize * 4;
    if rgba.len() < expected {
        return Err(screenshot_error("pixel_buffer_too_small"));
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|_| screenshot_error("png_header_failed"))?;
        writer
            .write_image_data(&rgba[..expected])
            .map_err(|_| screenshot_error("png_write_failed"))?;
        writer
            .finish()
            .map_err(|_| screenshot_error("png_finish_failed"))?;
    }
    Ok(out)
}

// —— Win32 GDI 最小 FFI(仅 Windows 编译)——

#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;
    use std::ptr::null_mut;

    pub type Hwnd = *mut c_void;
    pub type Hdc = *mut c_void;
    pub type Hbitmap = *mut c_void;
    pub type Hgdiobj = *mut c_void;

    pub const SM_XVIRTUALSCREEN: i32 = 76;
    pub const SM_YVIRTUALSCREEN: i32 = 77;
    pub const SM_CXVIRTUALSCREEN: i32 = 78;
    pub const SM_CYVIRTUALSCREEN: i32 = 79;
    pub const SRCCOPY: u32 = 0x00CC_0020;
    pub const DIB_RGB_COLORS: u32 = 0;
    pub const BI_RGB: u32 = 0;

    #[repr(C)]
    #[derive(Default)]
    pub struct BitmapInfoHeader {
        pub bi_size: u32,
        pub bi_width: i32,
        pub bi_height: i32,
        pub bi_planes: u16,
        pub bi_bit_count: u16,
        pub bi_compression: u32,
        pub bi_size_image: u32,
        pub bi_x_pels_per_meter: i32,
        pub bi_y_pels_per_meter: i32,
        pub bi_clr_used: u32,
        pub bi_clr_important: u32,
    }

    /// BITMAPINFO:32bpp 不需要调色板,结构体仍保留 1 个 RGBQUAD 占位。
    #[repr(C)]
    pub struct BitmapInfo {
        pub bmi_header: BitmapInfoHeader,
        pub bmi_colors: [u32; 1],
    }

    impl BitmapInfo {
        /// 自上而下(biHeight 为负)的 32bpp RGBA/BGRA 请求,避免再翻转一次行序。
        pub fn top_down_rgba(width: i32, height: i32) -> Self {
            Self {
                bmi_header: BitmapInfoHeader {
                    bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                    bi_width: width,
                    bi_height: -height,
                    bi_planes: 1,
                    bi_bit_count: 32,
                    bi_compression: BI_RGB,
                    ..BitmapInfoHeader::default()
                },
                bmi_colors: [0],
            }
        }
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn GetDC(hwnd: Hwnd) -> Hdc;
        pub fn ReleaseDC(hwnd: Hwnd, hdc: Hdc) -> i32;
        pub fn GetSystemMetrics(index: i32) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        pub fn CreateCompatibleDC(hdc: Hdc) -> Hdc;
        pub fn CreateCompatibleBitmap(hdc: Hdc, width: i32, height: i32) -> Hbitmap;
        pub fn SelectObject(hdc: Hdc, obj: Hgdiobj) -> Hgdiobj;
        pub fn BitBlt(
            dest: Hdc,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            src: Hdc,
            src_x: i32,
            src_y: i32,
            rop: u32,
        ) -> i32;
        pub fn GetDIBits(
            hdc: Hdc,
            bitmap: Hbitmap,
            start: u32,
            lines: u32,
            bits: *mut c_void,
            bmi: *mut BitmapInfo,
            usage: u32,
        ) -> i32;
        pub fn DeleteObject(obj: Hgdiobj) -> i32;
        pub fn DeleteDC(hdc: Hdc) -> i32;
    }

    /// 空句柄判断用。
    pub fn is_null<T>(handle: *mut T) -> bool {
        handle.is_null()
    }

    /// 屏幕 DC(null = 整个屏幕)。
    pub fn null_hwnd() -> Hwnd {
        null_mut()
    }
}

/// GDI 截图资源守卫:构造成功即持有,drop 时按依赖逆序释放。
#[cfg(windows)]
struct GdiCapture {
    screen_dc: win32::Hdc,
    mem_dc: win32::Hdc,
    bitmap: win32::Hbitmap,
    previous_bitmap: win32::Hgdiobj,
    bitmap_selected: bool,
}

#[cfg(windows)]
impl GdiCapture {
    fn new(width: i32, height: i32) -> Result<Self, AppError> {
        unsafe {
            let screen_dc = win32::GetDC(win32::null_hwnd());
            if win32::is_null(screen_dc) {
                return Err(screenshot_error("get_dc_failed"));
            }
            let mem_dc = win32::CreateCompatibleDC(screen_dc);
            if win32::is_null(mem_dc) {
                win32::ReleaseDC(win32::null_hwnd(), screen_dc);
                return Err(screenshot_error("create_mem_dc_failed"));
            }
            let bitmap = win32::CreateCompatibleBitmap(screen_dc, width, height);
            if win32::is_null(bitmap) {
                win32::DeleteDC(mem_dc);
                win32::ReleaseDC(win32::null_hwnd(), screen_dc);
                return Err(screenshot_error("create_bitmap_failed"));
            }
            let previous_bitmap = win32::SelectObject(mem_dc, bitmap);
            if win32::is_null(previous_bitmap) {
                win32::DeleteObject(bitmap);
                win32::DeleteDC(mem_dc);
                win32::ReleaseDC(win32::null_hwnd(), screen_dc);
                return Err(screenshot_error("select_bitmap_failed"));
            }
            Ok(Self {
                screen_dc,
                mem_dc,
                bitmap,
                previous_bitmap,
                bitmap_selected: true,
            })
        }
    }

    /// 把虚拟屏幕区域复制到内存位图。虚拟屏幕原点可为负(主屏左侧/上方的显示器)。
    fn copy_from_screen(
        &mut self,
        origin_x: i32,
        origin_y: i32,
        width: i32,
        height: i32,
    ) -> Result<(), AppError> {
        let copied = unsafe {
            win32::BitBlt(
                self.mem_dc,
                0,
                0,
                width,
                height,
                self.screen_dc,
                origin_x,
                origin_y,
                win32::SRCCOPY,
            )
        };
        if copied == 0 {
            return Err(screenshot_error("bitblt_failed"));
        }
        Ok(())
    }

    /// 读取 32bpp 像素(自上而下),返回 (buffer, stride)。
    ///
    /// `GetDIBits` 要求位图当时未被选入 DC,因此先把原选入对象还原。
    fn read_bgra(&mut self, width: u32, height: u32) -> Result<(Vec<u8>, usize), AppError> {
        if self.bitmap_selected {
            unsafe {
                win32::SelectObject(self.mem_dc, self.previous_bitmap);
            }
            self.bitmap_selected = false;
        }
        // 32bpp 行字节数 = width*4,已天然 4 字节对齐;仍按公式计算以防未来位深变化。
        let stride = (width as usize * 32).div_ceil(32) * 4;
        let mut buffer = vec![0u8; stride * height as usize];
        let mut bmi = win32::BitmapInfo::top_down_rgba(width as i32, height as i32);
        let lines = unsafe {
            win32::GetDIBits(
                self.mem_dc,
                self.bitmap,
                0,
                height,
                buffer.as_mut_ptr().cast(),
                &mut bmi,
                win32::DIB_RGB_COLORS,
            )
        };
        if lines == 0 {
            return Err(screenshot_error("get_dibits_failed"));
        }
        Ok((buffer, stride))
    }
}

#[cfg(windows)]
impl Drop for GdiCapture {
    fn drop(&mut self) {
        unsafe {
            if !self.bitmap.is_null() {
                if self.bitmap_selected {
                    win32::SelectObject(self.mem_dc, self.previous_bitmap);
                }
                win32::DeleteObject(self.bitmap);
            }
            if !self.mem_dc.is_null() {
                win32::DeleteDC(self.mem_dc);
            }
            if !self.screen_dc.is_null() {
                win32::ReleaseDC(win32::null_hwnd(), self.screen_dc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_serializes_camel_case() {
        let result = CaptureScreenResult {
            data_url: "data:image/png;base64,AA==".to_string(),
            width: 1920,
            height: 1080,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["width"], 1920);
        assert_eq!(json["height"], 1080);
        assert_eq!(json["dataUrl"], "data:image/png;base64,AA==");
        assert!(json.get("data_url").is_none());
    }

    #[test]
    fn bgra_rows_become_rgba_with_opaque_alpha() {
        // 两行,stride 比实际行多 4 字节 padding,验证按 stride 跳行。
        let stride = 12;
        let bgra = vec![
            // 行 1:纯蓝、纯绿
            255, 0, 0, 0, 0, 255, 0, 0, 9, 9, 9, 9, // 行 2:纯红、白
            0, 0, 255, 0, 255, 255, 255, 0, 9, 9, 9, 9,
        ];
        let rgba = bgra_to_rgba(&bgra, 2, 2, stride);
        assert_eq!(
            rgba,
            vec![0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255]
        );
    }

    #[test]
    fn bgra_short_buffer_does_not_panic() {
        // 缓冲区不足一整行时宁可少写(跳过该行),也不能索引越界。
        let rgba = bgra_to_rgba(&[1, 2, 3, 4], 2, 2, 8);
        assert_eq!(rgba.len(), 16);
        assert_eq!(&rgba[..4], &[0, 0, 0, 0]);
        assert_eq!(&rgba[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn png_encoding_produces_png_signature() {
        let rgba = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let png = encode_png(&rgba, 2, 2).unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR 宽高字段(大端)应等于入参。
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 2);
        assert_eq!(u32::from_be_bytes([png[20], png[21], png[22], png[23]]), 2);
    }

    #[test]
    fn encode_png_rejects_empty_or_short_buffers() {
        assert!(encode_png(&[], 0, 10).is_err());
        assert!(encode_png(&[], 10, 0).is_err());
        assert!(encode_png(&[0u8; 8], 2, 2).is_err());
    }

    #[test]
    fn base64_data_url_is_embedded() {
        // 与命令返回值同一拼装方式:PNG → base64 → data:image/png。
        let png = encode_png(&[9u8; 16], 2, 2).unwrap();
        let data_url = format!(
            "data:image/png;base64,{}",
            crate::shared::dto::base64_encode(&png)
        );
        assert!(data_url.starts_with("data:image/png;base64,iVBOR"));
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_reports_unsupported() {
        let error = capture_virtual_screen().unwrap_err();
        assert_eq!(error.code, "INTERNAL");
        assert_eq!(
            error.details.as_ref().and_then(|d| d.get("reason")),
            Some(&crate::shared::error::DetailValue::Str(
                "capture_unsupported_platform".to_string()
            ))
        );
    }

    /// 真实 GDI 捕获冒烟测试(需要已登录的桌面会话,默认忽略)。
    /// 手动运行:`cargo test --lib real_screen_capture -- --ignored`
    #[cfg(windows)]
    #[test]
    #[ignore = "需要真实桌面会话;手动运行 cargo test --lib real_screen_capture -- --ignored"]
    fn real_screen_capture_returns_png_data_url() {
        use base64::Engine;
        let result = capture_virtual_screen().expect("GDI 虚拟屏幕捕获应成功");
        assert!(result.width > 0 && result.height > 0, "尺寸为正");
        let prefix = "data:image/png;base64,";
        assert!(result.data_url.starts_with(prefix));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&result.data_url[prefix.len()..])
            .expect("base64 可解码");
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
        // PNG IHDR 中的宽高必须与返回值一致(证明 row padding/尺寸换算正确)。
        assert_eq!(
            u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            result.width
        );
        assert_eq!(
            u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
            result.height
        );
        assert!(bytes.len() > 64, "PNG 应包含实际像素数据");
    }
}
