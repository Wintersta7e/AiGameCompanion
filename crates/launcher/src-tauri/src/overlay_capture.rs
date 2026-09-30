//! Single-frame Windows Graphics Capture for the external overlay companion.

/// Capture `hwnd` as PNG, but only while it is still a live window of `pid`.
///
/// The only way to capture from outside this module. The check runs here, in
/// the caller's blocking task, immediately before the capture: a target that
/// closed or was recycled since the request was accepted is never captured.
pub(crate) fn capture_live_window_png(hwnd: i64, pid: u32) -> Result<Vec<u8>, String> {
    if !crate::overlay::is_live_window(hwnd, pid) {
        return Err("target window is gone".to_owned());
    }
    capture_window_png(hwnd)
}

#[cfg(windows)]
fn capture_window_png(hwnd: i64) -> Result<Vec<u8>, String> {
    imp::capture_window_png(hwnd)
}

#[cfg(not(windows))]
fn capture_window_png(_hwnd: i64) -> Result<Vec<u8>, String> {
    Err("screen capture is only supported on Windows".into())
}

/// Scale an RGBA frame down to fit inside `max_w` x `max_h`, keeping its aspect
/// ratio. Each output pixel is the average of the block of source pixels it
/// covers (an area filter); the blocks tile the frame, so every source pixel
/// counts exactly once. A frame that already fits comes back unchanged.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(
        dead_code,
        reason = "only the Windows capture calls it; its tests run everywhere"
    )
)]
fn fit_within(rgba: &[u8], w: u32, h: u32, max_w: u32, max_h: u32) -> (Vec<u8>, u32, u32) {
    let (src_w, src_h) = (w as usize, h as usize);
    let fits = w <= max_w && h <= max_h;
    // An empty or mis-sized buffer is left for the PNG encoder to reject.
    if fits || src_w == 0 || src_h == 0 || rgba.len() != src_w * src_h * 4 {
        return (rgba.to_vec(), w, h);
    }
    let (out_w, out_h) = fitted_size(w, h, max_w, max_h);
    let (dst_w, dst_h) = (out_w as usize, out_h as usize);

    let mut out = Vec::with_capacity(dst_w * dst_h * 4);
    for oy in 0..dst_h {
        let (y0, y1) = (oy * src_h / dst_h, (oy + 1) * src_h / dst_h);
        for ox in 0..dst_w {
            let (x0, x1) = (ox * src_w / dst_w, (ox + 1) * src_w / dst_w);
            let mut sum = [0_usize; 4];
            for y in y0..y1 {
                let row = &rgba[(y * src_w + x0) * 4..(y * src_w + x1) * 4];
                for px in row.as_chunks::<4>().0 {
                    for (acc, &channel) in sum.iter_mut().zip(px) {
                        *acc += usize::from(channel);
                    }
                }
            }
            let count = (y1 - y0) * (x1 - x0);
            for acc in sum {
                out.push(u8::try_from((acc + count / 2) / count).unwrap_or(u8::MAX));
            }
        }
    }
    (out, out_w, out_h)
}

/// The largest size inside `max_w` x `max_h` with the aspect ratio of a
/// `w` x `h` frame that is larger than the box, rounded to whole pixels and at
/// least 1x1. Never larger than the frame on either side.
fn fitted_size(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let (w, h, max_w, max_h) = (
        u64::from(w),
        u64::from(h),
        u64::from(max_w),
        u64::from(max_h),
    );
    // The side that overflows the box by the larger factor sets the scale.
    let (out_w, out_h) = if w * max_h >= h * max_w {
        (max_w, (h * max_w + w / 2) / w)
    } else {
        ((w * max_h + h / 2) / h, max_h)
    };
    let side = |value: u64| u32::try_from(value.max(1)).unwrap_or(u32::MAX);
    (side(out_w), side(out_h))
}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};

    use windows::core::{factory, Interface};
    use windows::Graphics::Capture::{
        Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
    };
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Foundation::{HMODULE, HWND};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
        D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

    const FRAME_TIMEOUT: Duration = Duration::from_secs(2);
    const FRAME_POLL_INTERVAL: Duration = Duration::from_millis(16);
    /// Largest frame sent to a provider. A 4K frame encodes to a PNG over
    /// Gemini's 20 MB request limit; 1080p keeps on-screen text legible.
    const MAX_SENT_WIDTH: u32 = 1920;
    const MAX_SENT_HEIGHT: u32 = 1080;

    pub(super) fn capture_window_png(hwnd: i64) -> Result<Vec<u8>, String> {
        let (d3d_device, d3d_context, capture_device) = create_device()?;
        let item = create_capture_item(hwnd)?;
        let size = item
            .Size()
            .map_err(|error| format!("failed to read capture size: {error}"))?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err(format!(
                "capture window has invalid size {}x{}",
                size.Width, size.Height
            ));
        }

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &capture_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            1,
            size,
        )
        .map_err(|error| format!("failed to create capture frame pool: {error}"))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|error| format!("failed to create capture session: {error}"))?;

        // Suppress the yellow WGC capture border, which otherwise flashes on the
        // game on every translate/attach. Unsupported before Win10 2104, so a
        // failure here is cosmetic and must not fail the capture.
        if let Err(error) = session.SetIsBorderRequired(false) {
            tracing::debug!("could not disable the capture border: {error}");
        }

        let result = session
            .StartCapture()
            .map_err(|error| format!("failed to start capture: {error}"))
            .and_then(|()| capture_first_frame(&pool, &d3d_device, &d3d_context));

        crate::util::log_if_err("close the capture session", session.Close());
        crate::util::log_if_err("close the capture frame pool", pool.Close());
        result
    }

    fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext, IDirect3DDevice), String> {
        let mut device = None;
        let mut context = None;
        // SAFETY: both out-parameters live for the whole call and are checked
        // for None below; D3D11 reports any failure through the HRESULT.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                Some(&raw mut context),
            )
            .map_err(|error| format!("failed to create D3D11 device: {error}"))?;
        }
        let device = device.ok_or_else(|| "D3D11 returned no device".to_owned())?;
        let context = context.ok_or_else(|| "D3D11 returned no device context".to_owned())?;
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|error| format!("failed to get DXGI device: {error}"))?;
        // SAFETY: `dxgi_device` is a live COM interface obtained just above.
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
            .map_err(|error| format!("failed to create WinRT D3D11 device: {error}"))?;
        let capture_device = inspectable
            .cast::<IDirect3DDevice>()
            .map_err(|error| format!("failed to cast WinRT D3D11 device: {error}"))?;
        Ok((device, context, capture_device))
    }

    fn create_capture_item(hwnd: i64) -> Result<GraphicsCaptureItem, String> {
        let native_hwnd = usize::try_from(hwnd)
            .map(|handle| HWND(handle as *mut core::ffi::c_void))
            .map_err(|error| format!("invalid game window handle: {error}"))?;
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|error| format!("failed to get capture item factory: {error}"))?;
        // SAFETY: a stale window handle is rejected by the interop factory with
        // an error HRESULT rather than being dereferenced.
        unsafe { interop.CreateForWindow(native_hwnd) }
            .map_err(|error| format!("failed to create capture item: {error}"))
    }

    fn capture_first_frame(
        pool: &Direct3D11CaptureFramePool,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
    ) -> Result<Vec<u8>, String> {
        let frame = wait_for_frame(pool)?;
        let result = read_frame_png(&frame, device, context);
        crate::util::log_if_err("close the captured frame", frame.Close());
        result
    }

    fn wait_for_frame(pool: &Direct3D11CaptureFramePool) -> Result<Direct3D11CaptureFrame, String> {
        let started = Instant::now();
        loop {
            if let Ok(frame) = pool.TryGetNextFrame() {
                return Ok(frame);
            }
            if started.elapsed() >= FRAME_TIMEOUT {
                return Err("timed out waiting for the first capture frame".to_owned());
            }
            std::thread::sleep(FRAME_POLL_INTERVAL);
        }
    }

    fn read_frame_png(
        frame: &Direct3D11CaptureFrame,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
    ) -> Result<Vec<u8>, String> {
        let surface = frame
            .Surface()
            .map_err(|error| format!("failed to get capture surface: {error}"))?;
        let access = surface
            .cast::<IDirect3DDxgiInterfaceAccess>()
            .map_err(|error| format!("failed to access capture DXGI surface: {error}"))?;
        // SAFETY: `access` is the DXGI interface of the frame surface we still
        // hold, and the requested interface type is checked by QueryInterface.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|error| format!("failed to get capture texture: {error}"))?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` is a live, correctly sized out-parameter on this frame.
        unsafe { texture.GetDesc(&raw mut desc) };
        if desc.Width == 0 || desc.Height == 0 {
            return Err("capture frame has an empty texture".to_owned());
        }

        let mut staging_desc = desc;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.BindFlags = 0;
        staging_desc.CPUAccessFlags = u32::try_from(D3D11_CPU_ACCESS_READ.0)
            .map_err(|error| format!("invalid D3D11 CPU access flag: {error}"))?;
        staging_desc.MiscFlags = 0;

        let mut staging = None;
        // SAFETY: the descriptor and the out-parameter live for the whole call,
        // and a None result is treated as a failure below.
        unsafe {
            device
                .CreateTexture2D(&raw const staging_desc, None, Some(&raw mut staging))
                .map_err(|error| format!("failed to create staging texture: {error}"))?;
        }
        let staging = staging.ok_or_else(|| "D3D11 returned no staging texture".to_owned())?;
        // SAFETY: both textures are live and were created on `device`, and the
        // staging copy matches the source descriptor.
        unsafe { context.CopyResource(&staging, &texture) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: subresource 0 of a staging texture created with CPU read
        // access; the mapping is released by the Unmap below.
        unsafe {
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))
                .map_err(|error| format!("failed to map staging texture: {error}"))?;
        }
        let pixels = read_mapped_rgba(&mapped, desc.Width, desc.Height);
        // SAFETY: pairs with the Map above on the same texture and subresource.
        unsafe { context.Unmap(&staging, 0) };
        let (rgba, width, height) = super::fit_within(
            &pixels?,
            desc.Width,
            desc.Height,
            MAX_SENT_WIDTH,
            MAX_SENT_HEIGHT,
        );
        let png = encode_png(width, height, &rgba)?;
        tracing::info!(
            "Screenshot {width}x{height} (captured {}x{}) encoded to {} bytes",
            desc.Width,
            desc.Height,
            png.len()
        );
        Ok(png)
    }

    fn read_mapped_rgba(
        mapped: &D3D11_MAPPED_SUBRESOURCE,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        if mapped.pData.is_null() {
            return Err("mapped staging texture returned a null pointer".to_owned());
        }
        let width = usize::try_from(width)
            .map_err(|error| format!("capture width is too large: {error}"))?;
        let height = usize::try_from(height)
            .map_err(|error| format!("capture height is too large: {error}"))?;
        let row_bytes = width
            .checked_mul(4)
            .ok_or_else(|| "capture row size overflowed".to_owned())?;
        let row_pitch = usize::try_from(mapped.RowPitch)
            .map_err(|error| format!("capture row pitch is too large: {error}"))?;
        if row_pitch < row_bytes {
            return Err(format!(
                "capture row pitch {row_pitch} is smaller than row size {row_bytes}"
            ));
        }
        let mapped_len = row_pitch
            .checked_mul(height)
            .ok_or_else(|| "mapped capture size overflowed".to_owned())?;
        let pixel_len = row_bytes
            .checked_mul(height)
            .ok_or_else(|| "capture pixel size overflowed".to_owned())?;
        // SAFETY: `pData` points at `RowPitch * height` bytes owned by the live
        // mapping, and `mapped_len` is that product, checked for overflow above.
        let source = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), mapped_len) };
        let mut rgba = Vec::with_capacity(pixel_len);
        for row in 0..height {
            let offset = row
                .checked_mul(row_pitch)
                .ok_or_else(|| "capture row offset overflowed".to_owned())?;
            for bgra in source[offset..offset + row_bytes].as_chunks::<4>().0 {
                rgba.extend_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
            }
        }
        Ok(rgba)
    }

    fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
        let mut output = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut output, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|error| format!("failed to write PNG header: {error}"))?;
            writer
                .write_image_data(rgba)
                .map_err(|error| format!("failed to encode PNG: {error}"))?;
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::fit_within;

    /// A frame of `w` x `h` pixels, every one of them `pixel`.
    fn flat_frame(w: u32, h: u32, pixel: [u8; 4]) -> Vec<u8> {
        pixel.repeat(w as usize * h as usize)
    }

    #[test]
    fn fit_within_scales_large_frames() {
        for ((w, h), expected) in [((3840, 2160), (1920, 1080)), ((2560, 1080), (1920, 810))] {
            let (out, out_w, out_h) =
                fit_within(&flat_frame(w, h, [10, 20, 30, 255]), w, h, 1920, 1080);
            println!("{w}x{h} -> {out_w}x{out_h}, {} bytes", out.len());
            assert_eq!((out_w, out_h), expected);
            assert_eq!(out.len(), out_w as usize * out_h as usize * 4);
            assert!(
                out.as_chunks::<4>()
                    .0
                    .iter()
                    .all(|px| *px == [10, 20, 30, 255]),
                "a flat frame must stay flat"
            );
        }
    }

    #[test]
    fn fit_within_averages_each_block() {
        // 4x2 into 2x1: each output pixel is the mean of one 2x2 block.
        let rgba = [
            0, 0, 0, 255, 100, 100, 100, 255, 10, 0, 0, 0, 30, 0, 0, 0, //
            0, 0, 0, 255, 100, 100, 100, 255, 50, 0, 0, 0, 70, 0, 0, 0,
        ];
        let (out, w, h) = fit_within(&rgba, 4, 2, 2, 1);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, [50, 50, 50, 255, 40, 0, 0, 0]);
    }

    #[test]
    fn fit_within_leaves_small_frames_alone() {
        for (w, h) in [(1280, 720), (1920, 1080), (1, 1)] {
            let rgba: Vec<u8> = (0..=u8::MAX)
                .cycle()
                .take(w as usize * h as usize * 4)
                .collect();
            let (out, out_w, out_h) = fit_within(&rgba, w, h, 1920, 1080);
            assert_eq!((out_w, out_h), (w, h));
            assert_eq!(out, rgba, "{w}x{h} must come back unchanged");
        }
    }

    #[test]
    fn fit_within_keeps_the_aspect_of_odd_sizes() {
        for (w, h) in [
            (3001, 1999),
            (1921, 1081),
            (2561, 1439),
            (5000, 3),
            (3, 5000),
        ] {
            let (out, out_w, out_h) = fit_within(&flat_frame(w, h, [1, 2, 3, 4]), w, h, 1920, 1080);
            println!("{w}x{h} -> {out_w}x{out_h}");
            assert!((1..=1920).contains(&out_w) && (1..=1080).contains(&out_h));
            assert!(out_w == 1920 || out_h == 1080, "one side must fill the box");
            let exact_h = f64::from(h) * f64::from(out_w) / f64::from(w);
            let exact_w = f64::from(w) * f64::from(out_h) / f64::from(h);
            assert!(
                (f64::from(out_h) - exact_h).abs() <= 1.0
                    || (f64::from(out_w) - exact_w).abs() <= 1.0,
                "{w}x{h} -> {out_w}x{out_h} changed the aspect ratio"
            );
            assert_eq!(out.len(), out_w as usize * out_h as usize * 4);
        }
    }
}
