//! Single-frame Windows Graphics Capture for the external overlay companion.

#[cfg(windows)]
pub fn capture_window_png(hwnd: i64) -> Result<Vec<u8>, String> {
    imp::capture_window_png(hwnd)
}

#[cfg(not(windows))]
pub fn capture_window_png(_hwnd: i64) -> Result<Vec<u8>, String> {
    Err("screen capture is only supported on Windows".into())
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

    pub fn capture_window_png(hwnd: i64) -> Result<Vec<u8>, String> {
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

        let result = session
            .StartCapture()
            .map_err(|error| format!("failed to start capture: {error}"))
            .and_then(|()| capture_first_frame(&pool, &d3d_device, &d3d_context));

        let _ = session.Close();
        let _ = pool.Close();
        result
    }

    fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext, IDirect3DDevice), String> {
        let mut device = None;
        let mut context = None;
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
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
            .map_err(|error| format!("failed to create WinRT D3D11 device: {error}"))?;
        let capture_device = inspectable
            .cast::<IDirect3DDevice>()
            .map_err(|error| format!("failed to cast WinRT D3D11 device: {error}"))?;
        Ok((device, context, capture_device))
    }

    fn create_capture_item(hwnd: i64) -> Result<GraphicsCaptureItem, String> {
        let native_hwnd = isize::try_from(hwnd)
            .map(HWND)
            .map_err(|error| format!("invalid game window handle: {error}"))?;
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|error| format!("failed to get capture item factory: {error}"))?;
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
        let _ = frame.Close();
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
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|error| format!("failed to get capture texture: {error}"))?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
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
        unsafe {
            device
                .CreateTexture2D(&raw const staging_desc, None, Some(&raw mut staging))
                .map_err(|error| format!("failed to create staging texture: {error}"))?;
        }
        let staging = staging.ok_or_else(|| "D3D11 returned no staging texture".to_owned())?;
        unsafe { context.CopyResource(&staging, &texture) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))
                .map_err(|error| format!("failed to map staging texture: {error}"))?;
        }
        let pixels = read_mapped_rgba(&mapped, desc.Width, desc.Height);
        unsafe { context.Unmap(&staging, 0) };
        encode_png(desc.Width, desc.Height, &pixels?)
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
        let source = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), mapped_len) };
        let mut rgba = Vec::with_capacity(pixel_len);
        for row in 0..height {
            let offset = row
                .checked_mul(row_pitch)
                .ok_or_else(|| "capture row offset overflowed".to_owned())?;
            for bgra in source[offset..offset + row_bytes].chunks_exact(4) {
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
