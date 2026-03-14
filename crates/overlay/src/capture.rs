use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use image::imageops::FilterType;
use image::RgbaImage;
use std::io::Cursor;
use tracing::warn;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetDC,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};

use crate::config::CONFIG;

/// Capture the foreground window via GDI BitBlt from the screen DC.
/// Uses screen DC so DWM-composited DX12 content is captured correctly.
/// Returns None on failure (non-fatal -- caller sends text-only).
pub fn capture_screenshot() -> Option<String> {
    unsafe { capture_gdi() }
}

unsafe fn capture_gdi() -> Option<String> {
    let hwnd = GetForegroundWindow();
    if hwnd.0 == 0 {
        warn!("Screenshot failed: no foreground window");
        return None;
    }

    // Try DWM extended frame bounds first (excludes shadow/chrome),
    // fall back to GetWindowRect.
    let rect = get_window_bounds(hwnd)?;

    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        warn!("Screenshot failed: zero dimensions ({width}x{height})");
        return None;
    }
    let (width, height) = (width as u32, height as u32);

    // Get the SCREEN DC (null HWND) -- captures DWM-composited content including DX12
    let hdc_screen = GetDC(HWND(0));
    if hdc_screen.is_invalid() {
        warn!("Screenshot failed: GetDC(screen) returned invalid handle");
        return None;
    }

    // Create compatible memory DC and bitmap
    let hdc_mem = CreateCompatibleDC(hdc_screen);
    if hdc_mem.is_invalid() {
        ReleaseDC(HWND(0), hdc_screen);
        warn!("Screenshot failed: CreateCompatibleDC failed");
        return None;
    }

    let hbitmap = CreateCompatibleBitmap(hdc_screen, width as i32, height as i32);
    if hbitmap.is_invalid() {
        DeleteDC(hdc_mem);
        ReleaseDC(HWND(0), hdc_screen);
        warn!("Screenshot failed: CreateCompatibleBitmap failed");
        return None;
    }

    // Select bitmap into memory DC
    let old_object = SelectObject(hdc_mem, hbitmap);

    // BitBlt from screen DC at window position to memory DC
    let blt_ok = BitBlt(
        hdc_mem,
        0,
        0,
        width as i32,
        height as i32,
        hdc_screen,
        rect.left,
        rect.top,
        SRCCOPY,
    );

    if blt_ok.is_err() {
        SelectObject(hdc_mem, old_object);
        DeleteDC(hdc_mem);
        ReleaseDC(HWND(0), hdc_screen);
        DeleteObject(hbitmap);
        warn!("Screenshot failed: BitBlt failed");
        return None;
    }

    // Read pixel data via GetDIBits (negative biHeight = top-down)
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..std::mem::zeroed()
        },
        ..std::mem::zeroed()
    };

    let mut pixels = vec![0u8; (width * height * 4) as usize];

    let lines = GetDIBits(
        hdc_mem,
        hbitmap,
        0,
        height,
        Some(pixels.as_mut_ptr() as *mut _),
        &mut bmi,
        DIB_RGB_COLORS,
    );

    // Cleanup GDI handles in correct order
    SelectObject(hdc_mem, old_object);
    DeleteDC(hdc_mem);
    ReleaseDC(HWND(0), hdc_screen);
    DeleteObject(hbitmap);

    if lines == 0 {
        warn!("Screenshot failed: GetDIBits returned 0 lines");
        return None;
    }

    // BGRA -> RGBA swap
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    // Build image
    let Some(mut img) = RgbaImage::from_raw(width, height, pixels) else {
        warn!("Screenshot failed: could not create image from raw pixels");
        return None;
    };

    // Downscale if wider than max_width
    let max_width = CONFIG.capture.max_width;
    if width > max_width {
        let new_height = (height as f64 * max_width as f64 / width as f64) as u32;
        img = image::imageops::resize(&img, max_width, new_height, FilterType::Triangle);
    }

    // Encode to PNG
    let mut png_buf = Cursor::new(Vec::new());
    if img.write_to(&mut png_buf, image::ImageFormat::Png).is_err() {
        warn!("Screenshot failed: PNG encoding failed");
        return None;
    }

    // Base64 encode
    Some(STANDARD.encode(png_buf.into_inner()))
}

/// Get the visible window bounds, preferring DWM extended frame bounds
/// (excludes shadows and invisible borders) over GetWindowRect.
unsafe fn get_window_bounds(hwnd: HWND) -> Option<RECT> {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};

    // Try DWM first -- gives exact visible bounds without shadow
    let mut rect: RECT = std::mem::zeroed();
    let hr = DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut rect as *mut RECT as *mut _,
        std::mem::size_of::<RECT>() as u32,
    );
    if hr.is_ok() {
        return Some(rect);
    }

    // Fallback to GetWindowRect (includes shadow on Win10/11)
    if GetWindowRect(hwnd, &mut rect).is_ok() {
        Some(rect)
    } else {
        warn!("Screenshot failed: GetWindowRect failed");
        None
    }
}
