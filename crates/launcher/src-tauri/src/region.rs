//! Rectangles on a captured frame, and the part of a game's window the user
//! keeps for translation.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::overlay::GameInfo;
use crate::shots::{Frame, floor_px};
use crate::state::AppState;

/// A rectangle in a frame's pixels, its origin at the frame's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PixelRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// The smallest side, in source pixels, a region may map to.
pub(crate) const MIN_REGION_PX: u32 = 16;
/// How far past the window's right or bottom edge a stored region may reach,
/// for the rounding of the fractions it was stored as.
const EDGE_SLACK: f64 = 1e-9;
/// Keeps an edge that falls on a whole pixel from rounding a pixel outward.
const ROUNDING_SLACK: f64 = 1e-6;

/// The part of a game's window a translation keeps: fractions of the window's
/// client area, so it holds across window modes and resolutions. Only a region
/// inside the window loads; anything else fails its entry.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRegion")]
pub(crate) struct TranslateRegion {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// A region as stored, before it is checked.
#[derive(Deserialize)]
struct RawRegion {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl TryFrom<RawRegion> for TranslateRegion {
    type Error = &'static str;

    fn try_from(raw: RawRegion) -> Result<Self, Self::Error> {
        let RawRegion { x, y, w, h } = raw;
        let finite = [x, y, w, h].iter().all(|value| value.is_finite());
        let inside = x >= 0.0
            && y >= 0.0
            && w > 0.0
            && h > 0.0
            && x + w <= 1.0 + EDGE_SLACK
            && y + h <= 1.0 + EDGE_SLACK;
        if finite && inside {
            Ok(Self { x, y, w, h })
        } else {
            Err("not a region inside the window")
        }
    }
}

/// Why a stored region cannot be applied to a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegionError {
    /// The frame does not say where the window's client area lies.
    NoClientBox,
    /// The region maps to a side under `MIN_REGION_PX`.
    TooSmall,
}

/// `region` in the pixels of a frame whose client area is `client_box`: each
/// start rounded down, each end rounded up, clamped to the client area.
pub(crate) fn region_to_pixels(
    region: &TranslateRegion,
    client_box: PixelRect,
) -> Result<PixelRect, RegionError> {
    let (x, width) = span(region.x, region.w, client_box.x, client_box.width);
    let (y, height) = span(region.y, region.h, client_box.y, client_box.height);
    if width < MIN_REGION_PX || height < MIN_REGION_PX {
        return Err(RegionError::TooSmall);
    }
    Ok(PixelRect {
        x,
        y,
        width,
        height,
    })
}

/// The fraction `start .. start + len` of a side `len_px` pixels long that
/// begins at `base`, as (first pixel, pixel count).
fn span(start: f64, len: f64, base: u32, len_px: u32) -> (u32, u32) {
    let (low, high) = (f64::from(base), f64::from(base) + f64::from(len_px));
    let first = start
        .mul_add(f64::from(len_px), low + ROUNDING_SLACK)
        .floor();
    let end = (start + len)
        .mul_add(f64::from(len_px), low - ROUNDING_SLACK)
        .ceil();
    let (first, end) = (
        floor_px(first.clamp(low, high)),
        floor_px(end.clamp(low, high)),
    );
    (first, end.saturating_sub(first))
}

/// What a take sends from `source`: the whole frame with no region, else the
/// region's crop. A region is never answered with the whole frame: a frame
/// with no client box, or a region that maps under the minimum size, is an
/// error.
pub(crate) fn framed(
    source: &Frame,
    region: Option<&TranslateRegion>,
) -> Result<Frame, RegionError> {
    let Some(region) = region else {
        return Ok(source.clone());
    };
    let client_box = source.client_box.ok_or(RegionError::NoClientBox)?;
    let rect = region_to_pixels(region, client_box)?;
    crop(source, rect).ok_or(RegionError::NoClientBox)
}

/// The pixels of `rect` copied out of `source`, row by row. `None` when the
/// rect does not lie inside the frame.
fn crop(source: &Frame, rect: PixelRect) -> Option<Frame> {
    let fits = rect.x.checked_add(rect.width)? <= source.width
        && rect.y.checked_add(rect.height)? <= source.height
        && rect.width > 0;
    if !fits {
        return None;
    }
    let stride = source.width as usize * 4;
    let (left, right) = (rect.x as usize * 4, (rect.x + rect.width) as usize * 4);
    let mut rgba = Vec::with_capacity((right - left) * rect.height as usize);
    for row in source
        .rgba
        .chunks_exact(stride)
        .skip(rect.y as usize)
        .take(rect.height as usize)
    {
        rgba.extend_from_slice(row.get(left..right)?);
    }
    (rgba.len() == (right - left) * rect.height as usize).then_some(Frame {
        rgba,
        width: rect.width,
        height: rect.height,
        client_box: None,
    })
}

/// The region stored for the game `key`. The one read of the stored value.
pub(crate) fn stored_region(state: &AppState, key: &str) -> Option<TranslateRegion> {
    state.game_prefs(key).translate_region
}

/// The region stored for `game`, when it is a linked game with a key.
pub(crate) fn linked_region(app: &AppHandle, game: Option<&GameInfo>) -> Option<TranslateRegion> {
    let key = crate::overlay::game_key(game?)?;
    let state = app.try_state::<AppState>()?;
    stored_region(&state, &key)
}

#[cfg(test)]
mod tests {
    use super::{MIN_REGION_PX, PixelRect, RegionError, TranslateRegion, framed, region_to_pixels};
    use crate::shots::Frame;
    use std::path::Path;

    fn region(json: &str) -> TranslateRegion {
        serde_json::from_str(json).unwrap()
    }

    fn byte(value: u32) -> u8 {
        u8::try_from(value).unwrap()
    }

    /// A `w` x `h` frame (both under 256) whose pixel at (x, y) is
    /// `[x, y, 0, 255]`.
    fn frame(w: u32, h: u32, client_box: Option<PixelRect>) -> Frame {
        let rgba = (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [byte(x), byte(y), 0, 255]))
            .collect();
        Frame {
            rgba,
            width: w,
            height: h,
            client_box,
        }
    }

    const BOX: PixelRect = PixelRect {
        x: 4,
        y: 6,
        width: 56,
        height: 36,
    };

    #[test]
    fn stored_region_never_widens() {
        let source = frame(64, 48, Some(BOX));
        let whole = framed(&source, None).unwrap();
        println!("no region: {}x{}", whole.width, whole.height);
        assert_eq!(
            (whole.width, whole.height),
            (64, 48),
            "no region: the source"
        );
        assert_eq!(whole.rgba, source.rgba, "no region: the same bytes");

        let half = region(r#"{"x":0.25,"y":0.25,"w":0.5,"h":0.5}"#);
        let no_box = frame(64, 48, None);
        let unboxed = framed(&no_box, Some(&half)).map(|f| (f.width, f.height));
        println!("no client box: {unboxed:?}");
        assert_eq!(
            unboxed,
            Err(RegionError::NoClientBox),
            "a region with no client box is never the whole frame"
        );
        let tiny = region(r#"{"x":0,"y":0,"w":0.1,"h":0.1}"#);
        assert_eq!(
            framed(&source, Some(&tiny)).map(|f| (f.width, f.height)),
            Err(RegionError::TooSmall),
            "a region under the minimum size"
        );

        let all = region(r#"{"x":0,"y":0,"w":1,"h":1}"#);
        for (case, stored) in [("half", &half), ("the whole client area", &all)] {
            let rect = region_to_pixels(stored, BOX).unwrap();
            let crop = framed(&source, Some(stored)).unwrap();
            println!("{case}: {rect:?} -> {}x{}", crop.width, crop.height);
            assert_eq!(
                (crop.width, crop.height),
                (rect.width, rect.height),
                "{case}"
            );
            assert_ne!(
                (crop.width, crop.height),
                (64, 48),
                "{case}: never the whole frame"
            );
            assert_eq!(crop.client_box, None, "{case}: a crop has no client box");
            let (right, bottom) = (rect.x + rect.width - 1, rect.y + rect.height - 1);
            let first = crop.rgba.get(..4).unwrap();
            let last = crop.rgba.get(crop.rgba.len() - 4..).unwrap();
            assert_eq!(
                first,
                [byte(rect.x), byte(rect.y), 0, 255],
                "{case}: top left"
            );
            assert_eq!(
                last,
                [byte(right), byte(bottom), 0, 255],
                "{case}: bottom right"
            );
        }
    }

    #[test]
    fn region_to_pixels_clamps_and_rejects() {
        let client = PixelRect {
            x: 8,
            y: 31,
            width: 1904,
            height: 1041,
        };
        let cases = [
            (
                "a quarter band",
                r#"{"x":0.25,"y":0.5,"w":0.5,"h":0.25}"#,
                Ok(PixelRect {
                    x: 484,
                    y: 551,
                    width: 952,
                    height: 261,
                }),
            ),
            (
                "the whole client area",
                r#"{"x":0,"y":0,"w":1,"h":1}"#,
                Ok(client),
            ),
            (
                "past the edge within the allowed slack",
                r#"{"x":0,"y":0,"w":1.0000000008,"h":1.0000000008}"#,
                Ok(client),
            ),
            (
                "too narrow",
                r#"{"x":0,"y":0,"w":0.005,"h":1}"#,
                Err(RegionError::TooSmall),
            ),
            (
                "too short",
                r#"{"x":0,"y":0.99,"w":1,"h":0.01}"#,
                Err(RegionError::TooSmall),
            ),
        ];
        for (case, json, expected) in cases {
            let got = region_to_pixels(&region(json), client);
            println!("{case}: {got:?}");
            assert_eq!(got, expected, "{case}");
        }
        let wider = PixelRect {
            width: 3200,
            ..client
        };
        let narrow = region_to_pixels(&region(r#"{"x":0,"y":0,"w":0.005,"h":1}"#), wider);
        println!("16 px wide: {narrow:?}");
        assert_eq!(
            narrow.map(|rect| rect.width),
            Ok(MIN_REGION_PX),
            "the minimum size is enough"
        );
    }

    #[test]
    fn invalid_regions_do_not_parse() {
        for json in [
            r#"{"x":-0.1,"y":0,"w":0.5,"h":0.5}"#,
            r#"{"x":0,"y":-0.1,"w":0.5,"h":0.5}"#,
            r#"{"x":0,"y":0,"w":0,"h":0.5}"#,
            r#"{"x":0,"y":0,"w":0.5,"h":0}"#,
            r#"{"x":0,"y":0,"w":0.5,"h":-0.5}"#,
            r#"{"x":0.5,"y":0,"w":0.6,"h":1}"#,
            r#"{"x":0,"y":0.5,"w":1,"h":0.6}"#,
            r#"{"x":0,"y":0,"w":0.5}"#,
            r#"{"x":"0","y":0,"w":0.5,"h":0.5}"#,
            "null",
        ] {
            let parsed = serde_json::from_str::<TranslateRegion>(json);
            println!("{json}: {parsed:?}");
            assert!(parsed.is_err(), "{json} parsed");
        }
        let valid = r#"{"x":0.25,"y":0.0,"w":0.75,"h":1.0}"#;
        let parsed = serde_json::from_str::<TranslateRegion>(valid).unwrap();
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            valid,
            "a valid region round-trips"
        );
    }

    /// Only `region.rs` reads the stored region; `models.rs` declares it.
    #[test]
    fn translate_region_has_one_reader_file() {
        let counts = crate::util::count_in_sources_by_file(concat!(".translate", "_region"));
        println!("{} files scanned", counts.len());
        assert!(!counts.is_empty(), "the source scan found no files");
        let mut outside = Vec::new();
        for (file, count) in &counts {
            if *count > 0 {
                println!("{}: {count}", file.display());
            }
            let allowed = file == Path::new("region.rs") || file == Path::new("models.rs");
            if *count > 0 && !allowed {
                outside.push((file.clone(), *count));
            }
        }
        let in_region = counts
            .iter()
            .find(|(file, _)| file == Path::new("region.rs"))
            .map_or(0, |(_, count)| *count);
        assert!(in_region > 0, "region.rs reads the stored region");
        assert!(
            outside.is_empty(),
            "translate_region field access outside region.rs and models.rs: {outside:?}"
        );
    }
}
