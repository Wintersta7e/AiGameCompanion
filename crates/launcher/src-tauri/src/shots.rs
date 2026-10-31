//! Screenshots: the one capture pipeline, and the sizes every sent image keeps.

use crate::placement::Rect;
use crate::region::PixelRect;

/// A sent image fits inside this box, aspect kept.
pub(crate) const SHOT_MAX_W: u32 = 1920;
pub(crate) const SHOT_MAX_H: u32 = 1080;
/// A sent PNG is at most this many bytes: under 4.67 MB as base64, inside the
/// strictest per-image limit a provider documents (5 MB).
pub(crate) const SHOT_MAX_BYTES: usize = 3_500_000;
/// The full-resolution frame kept for a take fits inside this box.
pub(crate) const SOURCE_MAX_W: u32 = 3840;
pub(crate) const SOURCE_MAX_H: u32 = 2160;

const TOO_LARGE: &str = "The screenshot is too large to send.";

/// One captured frame: unscaled RGBA, and where the game's client area lies
/// in it (`None` when it could not be read).
#[derive(Debug)]
pub(crate) struct Frame {
    pub(crate) rgba: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) client_box: Option<PixelRect>,
}

/// An image ready to send: an RGB PNG inside the size and byte limits.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub(crate) png: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Capture the live window `hwnd` of `pid`. The crate's one capture call.
pub(crate) fn grab_frame(hwnd: i64, pid: u32) -> Result<Frame, String> {
    crate::overlay_capture::capture_live_frame(hwnd, pid)
}

/// Size and encode a captured frame for sending. Returns the image and the
/// size of the source it was made from.
pub(crate) fn finish_frame(frame: Frame) -> Result<(Prepared, (u32, u32)), String> {
    let captured = (frame.width, frame.height);
    let source = source(frame);
    let prepared = prepare(&source.rgba, source.width, source.height)?;
    tracing::info!(
        "Screenshot {}x{} (captured {}x{}) encoded to {} bytes",
        prepared.width,
        prepared.height,
        captured.0,
        captured.1,
        prepared.png.len()
    );
    Ok((prepared, (source.width, source.height)))
}

/// `frame` fitted inside `SOURCE_MAX_W` x `SOURCE_MAX_H`, its client box
/// scaled by the same factors (start rounded down, end rounded up). A frame
/// that already fits comes back unchanged.
pub(crate) fn source(frame: Frame) -> Frame {
    if frame.width <= SOURCE_MAX_W && frame.height <= SOURCE_MAX_H {
        return frame;
    }
    let (rgba, width, height) = fit_within(
        &frame.rgba,
        frame.width,
        frame.height,
        SOURCE_MAX_W,
        SOURCE_MAX_H,
    );
    let client_box = frame.client_box.and_then(|b| {
        let (x0, x1) = scale_span(b.x, b.width, frame.width, width);
        let (y0, y1) = scale_span(b.y, b.height, frame.height, height);
        (x1 > x0 && y1 > y0).then(|| PixelRect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    });
    Frame {
        rgba,
        width,
        height,
        client_box,
    }
}

/// The span `start .. start + len` of a side `from` pixels long, scaled to a
/// side `to` pixels long: start rounded down, end rounded up, both clamped.
fn scale_span(start: u32, len: u32, from: u32, to: u32) -> (u32, u32) {
    let (from, to) = (u64::from(from.max(1)), u64::from(to));
    let end = u64::from(start) + u64::from(len);
    let low = (u64::from(start) * to / from).min(to);
    let high = (end * to).div_ceil(from).min(to);
    let side = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
    (side(low), side(high))
}

/// An RGBA frame as an image to send: fitted inside `SHOT_MAX_W` x
/// `SHOT_MAX_H`, encoded as an 8-bit RGB PNG. Over `SHOT_MAX_BYTES` it is
/// scaled down once more by the square root of the overshoot, with a 10%
/// margin, and re-encoded; still over is an error.
pub(crate) fn prepare(rgba: &[u8], w: u32, h: u32) -> Result<Prepared, String> {
    let (fitted, w1, h1) = fit_within(rgba, w, h, SHOT_MAX_W, SHOT_MAX_H);
    let png = encode_rgb(&fitted, w1, h1)?;
    if png.len() <= SHOT_MAX_BYTES {
        return Ok(Prepared {
            png,
            width: w1,
            height: h1,
        });
    }
    let bytes = |len: usize| f64::from(u32::try_from(len).unwrap_or(u32::MAX));
    let scale = (bytes(SHOT_MAX_BYTES) / bytes(png.len())).sqrt() * 0.9;
    let (max_w, max_h) = (
        floor_px(f64::from(w1) * scale).max(1),
        floor_px(f64::from(h1) * scale).max(1),
    );
    let (smaller, w2, h2) = fit_within(&fitted, w1, h1, max_w, max_h);
    let png = encode_rgb(&smaller, w2, h2)?;
    tracing::debug!("Screenshot over the byte cap, scaled to {w2}x{h2}");
    if png.len() > SHOT_MAX_BYTES {
        return Err(TOO_LARGE.to_owned());
    }
    Ok(Prepared {
        png,
        width: w2,
        height: h2,
    })
}

/// `value` rounded down to whole pixels.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a non-negative size no larger than the side it was scaled from"
)]
const fn floor_px(value: f64) -> u32 {
    value.floor() as u32
}

/// Encode RGBA pixels as an 8-bit RGB PNG, dropping alpha.
fn encode_rgb(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let rgb: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, _]| [r, g, b])
        .collect();
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| format!("failed to write PNG header: {error}"))?;
        writer
            .write_image_data(&rgb)
            .map_err(|error| format!("failed to encode PNG: {error}"))?;
    }
    Ok(output)
}

/// Where the client area `client` (screen px) lies in a `width` x `height`
/// frame whose top-left corner is at `frame_origin` on screen: the client
/// area minus the origin, clipped to the frame. `None` when either input is
/// unknown or nothing of the client area is inside the frame.
pub(crate) fn client_box(
    client: Option<Rect>,
    frame_origin: Option<(i32, i32)>,
    width: u32,
    height: u32,
) -> Option<PixelRect> {
    let (client, (left, top)) = (client?, frame_origin?);
    let clip = |start: i32, origin: i32, len: u32, side: u32| {
        let start = i64::from(start) - i64::from(origin);
        let end = start + i64::from(len);
        let side = i64::from(side);
        let (low, high) = (start.clamp(0, side), end.clamp(0, side));
        if high <= low {
            return None;
        }
        Some((u32::try_from(low).ok()?, u32::try_from(high - low).ok()?))
    };
    let (x, w) = clip(client.x, left, client.w, width)?;
    let (y, h) = clip(client.y, top, client.h, height)?;
    Some(PixelRect {
        x,
        y,
        width: w,
        height: h,
    })
}

/// Scale an RGBA frame down to fit inside `max_w` x `max_h`, keeping its aspect
/// ratio. Each output pixel is the average of the block of source pixels it
/// covers (an area filter); the blocks tile the frame, so every source pixel
/// counts exactly once. A frame that already fits comes back unchanged.
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
                let Some(row) = rgba.get((y * src_w + x0) * 4..(y * src_w + x1) * 4) else {
                    return (rgba.to_vec(), w, h);
                };
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

#[cfg(test)]
mod tests {
    use super::{client_box, fit_within, prepare, source, Frame};
    use super::{SHOT_MAX_BYTES, SHOT_MAX_H, SHOT_MAX_W, SOURCE_MAX_H, SOURCE_MAX_W};
    use crate::placement::Rect;
    use crate::region::PixelRect;

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

    /// The colour type byte of a PNG's `IHDR` chunk (2 = 8-bit RGB).
    fn colour_type(png: &[u8]) -> Option<u8> {
        png.get(25).copied()
    }

    #[test]
    fn prepare_bounds_every_sent_image() {
        for ((w, h), expected) in [
            ((3840, 2160), (1920, 1080)),
            ((2560, 1080), (1920, 810)),
            ((1280, 720), (1280, 720)),
        ] {
            let prepared = prepare(&flat_frame(w, h, [40, 80, 120, 255]), w, h).unwrap();
            println!(
                "{w}x{h} -> {}x{}, {} bytes, colour type {:?}",
                prepared.width,
                prepared.height,
                prepared.png.len(),
                colour_type(&prepared.png)
            );
            assert_eq!((prepared.width, prepared.height), expected);
            assert_eq!(colour_type(&prepared.png), Some(2), "{w}x{h} must be RGB");
            assert!(prepared.png.len() <= SHOT_MAX_BYTES);
        }

        // Noise does not compress: a full-size frame encodes over the byte cap
        // and must come back smaller, still RGB, still inside the box.
        let mut state: u32 = 0x9E37_79B9;
        let noise: Vec<u8> = (0..SHOT_MAX_W * SHOT_MAX_H * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state.to_le_bytes()[0]
            })
            .collect();
        let prepared = prepare(&noise, SHOT_MAX_W, SHOT_MAX_H).unwrap();
        println!(
            "noise {SHOT_MAX_W}x{SHOT_MAX_H} -> {}x{}, {} bytes (cap {SHOT_MAX_BYTES})",
            prepared.width,
            prepared.height,
            prepared.png.len()
        );
        assert!(prepared.png.len() <= SHOT_MAX_BYTES);
        assert!(prepared.width < SHOT_MAX_W && prepared.height < SHOT_MAX_H);
        assert_eq!(colour_type(&prepared.png), Some(2));
    }

    #[test]
    fn source_is_capped() {
        let big = Frame {
            rgba: flat_frame(5120, 2880, [1, 2, 3, 255]),
            width: 5120,
            height: 2880,
            client_box: Some(PixelRect {
                x: 8,
                y: 31,
                width: 5104,
                height: 2841,
            }),
        };
        let capped = source(big);
        println!(
            "5120x2880 -> {}x{}, box {:?}",
            capped.width, capped.height, capped.client_box
        );
        assert_eq!((capped.width, capped.height), (SOURCE_MAX_W, SOURCE_MAX_H));
        assert_eq!(capped.rgba.len(), 3840 * 2160 * 4);
        // x 8 * 0.75 = 6 floored, end 5112 * 0.75 = 3834; y 31 * 0.75 = 23.25
        // floored to 23, end 2872 * 0.75 = 2154.
        assert_eq!(
            capped.client_box,
            Some(PixelRect {
                x: 6,
                y: 23,
                width: 3828,
                height: 2131
            })
        );

        // Fractional edges: start 9 * 0.75 = 6.75 -> 6, end 5110 * 0.75 =
        // 3832.5 -> 3833; start 23.25 -> 23, end 2873 * 0.75 = 2154.75 -> 2155.
        let fractional = source(Frame {
            rgba: flat_frame(5120, 2880, [1, 2, 3, 255]),
            width: 5120,
            height: 2880,
            client_box: Some(PixelRect {
                x: 9,
                y: 31,
                width: 5101,
                height: 2842,
            }),
        });
        println!("fractional edges -> box {:?}", fractional.client_box);
        assert_eq!(
            fractional.client_box,
            Some(PixelRect {
                x: 6,
                y: 23,
                width: 3827,
                height: 2132
            })
        );

        let fits_box = PixelRect {
            x: 8,
            y: 31,
            width: 3824,
            height: 2121,
        };
        let fits = source(Frame {
            rgba: flat_frame(3840, 2160, [1, 2, 3, 255]),
            width: 3840,
            height: 2160,
            client_box: Some(fits_box),
        });
        println!(
            "3840x2160 -> {}x{}, box {:?}",
            fits.width, fits.height, fits.client_box
        );
        assert_eq!((fits.width, fits.height), (3840, 2160));
        assert_eq!(fits.client_box, Some(fits_box));
    }

    #[test]
    fn client_box_is_client_minus_frame_origin() {
        let rect = |x, y, w, h| Rect { x, y, w, h };
        let cases = [
            (
                "borderless",
                Some(rect(100, 50, 1920, 1080)),
                Some((100, 50)),
                (1920, 1080),
                Some(PixelRect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                }),
            ),
            (
                "titled",
                Some(rect(108, 81, 1920, 1080)),
                Some((100, 50)),
                (1936, 1119),
                Some(PixelRect {
                    x: 8,
                    y: 31,
                    width: 1920,
                    height: 1080,
                }),
            ),
            (
                "client wider than the frame",
                Some(rect(100, 50, 2000, 1080)),
                Some((100, 50)),
                (1920, 1080),
                Some(PixelRect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                }),
            ),
            (
                "no overlap",
                Some(rect(5000, 50, 100, 100)),
                Some((100, 50)),
                (1920, 1080),
                None,
            ),
            ("no client", None, Some((100, 50)), (1920, 1080), None),
            (
                "no frame origin",
                Some(rect(100, 50, 1920, 1080)),
                None,
                (1920, 1080),
                None,
            ),
        ];
        for (case, client, origin, (w, h), expected) in cases {
            let got = client_box(client, origin, w, h);
            println!("{case}: {got:?}");
            assert_eq!(got, expected, "{case}");
        }
    }

    #[test]
    fn one_capture_producer() {
        let producer = concat!("capture_live", "_frame(");
        let primitive = concat!("capture_window", "_frame(");
        let producers = crate::util::count_in_sources_by_file(producer);
        let primitives = crate::util::count_in_sources_by_file(primitive);
        println!("scanned {} files", producers.len());
        let outside = |rows: &[(std::path::PathBuf, usize)]| -> Vec<(String, usize)> {
            rows.iter()
                .filter(|(file, count)| *count > 0 && !file.ends_with("overlay_capture.rs"))
                .map(|(file, count)| (file.display().to_string(), *count))
                .collect()
        };
        let calls = outside(&producers);
        let raw = outside(&primitives);
        println!("{producer} outside overlay_capture.rs: {calls:?}");
        println!("{primitive} outside overlay_capture.rs: {raw:?}");
        assert_eq!(
            calls,
            [("shots.rs".to_owned(), 1)],
            "one caller, in shots.rs"
        );
        assert!(
            raw.is_empty(),
            "the raw capture is private to overlay_capture.rs"
        );
    }
}
