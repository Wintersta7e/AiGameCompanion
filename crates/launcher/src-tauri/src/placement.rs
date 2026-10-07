//! Where the overlay panel opens: pure geometry in physical screen pixels.
//!
//! No Tauri or Win32 type appears here, so every rule is tested on any host.

use serde::{Deserialize, Serialize};

/// A screen rectangle in physical pixels. The right and bottom edges are
/// exclusive (`x + w`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Rect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

/// Where the user last left the panel, and what it was placed against then.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct OverlayPlacement {
    /// The panel's `outer_position` and `inner_size`: what the setters set.
    pub(crate) panel: Rect,
    /// The rect the panel was placed against: the game's client area on its
    /// monitor, else the cursor monitor's work area.
    pub(crate) reference: Rect,
    /// The panel window's scale factor when recorded.
    pub(crate) scale: f64,
}

/// The panel's width when no position is remembered, in logical px.
pub(crate) const DEFAULT_WIDTH: f64 = 400.0;
/// The smallest panel that keeps the tab bar, the input and Send usable, in
/// logical px.
pub(crate) const MIN_WIDTH: f64 = 320.0;
pub(crate) const MIN_HEIGHT: f64 = 400.0;

/// A monitor's work area (the monitor minus the taskbar) and its scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MonitorArea {
    pub(crate) work: Rect,
    pub(crate) scale: f64,
}

/// Where to show the panel, and why there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    pub(crate) rect: Rect,
    pub(crate) from: PlacedFrom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlacedFrom {
    /// Nothing usable remembered: along the reference rect's right edge.
    Default,
    /// The remembered rect, unchanged.
    Restored,
    /// The remembered rect, moved and scaled with the reference rect.
    Moved,
}

impl PlacedFrom {
    /// The word the placement log line uses.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Restored => "restored",
            Self::Moved => "moved with the game",
        }
    }
}

impl Rect {
    /// The rect between two edges; `None` unless it has an area. Values beyond
    /// `i32`/`u32` saturate.
    pub(crate) fn from_edges(left: i64, top: i64, right: i64, bottom: i64) -> Option<Self> {
        (right > left && bottom > top).then(|| Self {
            x: saturate_i32(left),
            y: saturate_i32(top),
            w: saturate_u32(right.saturating_sub(left)),
            h: saturate_u32(bottom.saturating_sub(top)),
        })
    }

    /// The exclusive right edge.
    fn right(&self) -> i64 {
        i64::from(self.x) + i64::from(self.w)
    }

    /// The exclusive bottom edge.
    fn bottom(&self) -> i64 {
        i64::from(self.y) + i64::from(self.h)
    }

    pub(crate) fn centre(&self) -> (f64, f64) {
        (
            f64::from(self.x) + f64::from(self.w) / 2.0,
            f64::from(self.y) + f64::from(self.h) / 2.0,
        )
    }

    fn intersect(&self, other: &Self) -> Option<Self> {
        Self::from_edges(
            i64::from(self.x.max(other.x)),
            i64::from(self.y.max(other.y)),
            self.right().min(other.right()),
            self.bottom().min(other.bottom()),
        )
    }

    /// Whether `other` lies fully inside this rect.
    fn contains_rect(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    fn contains_point(&self, (px, py): (f64, f64)) -> bool {
        let (x, y) = (f64::from(self.x), f64::from(self.y));
        x <= px && px < x + f64::from(self.w) && y <= py && py < y + f64::from(self.h)
    }
}

impl OverlayPlacement {
    /// Both rects have an area and the scale is a usable factor; anything else
    /// counts as no record.
    fn is_valid(&self) -> bool {
        [self.panel, self.reference]
            .iter()
            .all(|rect| rect.w > 0 && rect.h > 0)
            && self.scale.is_finite()
            && self.scale > 0.0
    }
}

/// `value` rounded to the nearest integer.
#[expect(
    clippy::cast_possible_truncation,
    reason = "an f64-to-i64 `as` saturates and maps NaN to 0, never UB"
)]
const fn round(value: f64) -> i64 {
    value.round() as i64
}

fn saturate_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

fn saturate_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(if value < 0 { 0 } else { u32::MAX })
}

fn same_scale(a: f64, b: f64) -> bool {
    (a - b).abs() < f64::EPSILON
}

/// The rect the panel is placed against, and its scale: the game's client
/// area cut to the work area of the monitor at its centre, else the work area
/// of the cursor's monitor, else nothing.
pub(crate) fn reference(
    client: Option<Rect>,
    at_centre: Option<MonitorArea>,
    at_cursor: Option<MonitorArea>,
) -> Option<(Rect, f64)> {
    client
        .zip(at_centre)
        .and_then(|(client, monitor)| Some((client.intersect(&monitor.work)?, monitor.scale)))
        .or_else(|| at_cursor.map(|monitor| (monitor.work, monitor.scale)))
}

/// Where to show the panel against the reference rect `r` (scale `r_scale`).
///
/// A remembered rect is restored unchanged while it was placed against the
/// same `r` and still lies inside a monitor of its scale; otherwise it keeps
/// its size and its distance from the nearer edges of `r`, both scaled. With
/// no usable record the panel runs along the right edge of `r`. A moved or
/// default rect is then fitted inside one monitor's work area.
pub(crate) fn place(
    saved: Option<&OverlayPlacement>,
    r: Rect,
    r_scale: f64,
    monitors: &[MonitorArea],
) -> Placed {
    let Some(saved) = saved.filter(|saved| saved.is_valid()) else {
        let w = round(DEFAULT_WIDTH * r_scale);
        let rect = Rect {
            x: saturate_i32(r.right().saturating_sub(w)),
            y: r.y,
            w: saturate_u32(w),
            h: r.h,
        };
        return Placed {
            rect: fit(rect, r, monitors),
            from: PlacedFrom::Default,
        };
    };
    let fits = monitors.iter().any(|monitor| {
        same_scale(monitor.scale, saved.scale) && monitor.work.contains_rect(&saved.panel)
    });
    if saved.reference == r && fits {
        return Placed {
            rect: saved.panel,
            from: PlacedFrom::Restored,
        };
    }
    Placed {
        rect: fit(re_anchor(saved, r, r_scale), r, monitors),
        from: PlacedFrom::Moved,
    }
}

/// The remembered panel moved onto `r`: its size scaled from the recorded
/// scale to `r_scale`, and on each axis its gap to the nearer edge of the old
/// reference rect kept, scaled the same way, from the same edge of `r`.
fn re_anchor(saved: &OverlayPlacement, r: Rect, r_scale: f64) -> Rect {
    let (panel, old) = (saved.panel, saved.reference);
    let scaled = |value: f64| round(value / saved.scale * r_scale);
    let width = scaled(f64::from(panel.w));
    let height = scaled(f64::from(panel.h));
    let (panel_x, panel_y) = panel.centre();
    let (old_x, old_y) = old.centre();
    let left = if panel_x >= old_x {
        let gap = f64::from(old.x) + f64::from(old.w) - f64::from(panel.x) - f64::from(panel.w);
        r.right().saturating_sub(scaled(gap)).saturating_sub(width)
    } else {
        i64::from(r.x).saturating_add(scaled(f64::from(panel.x) - f64::from(old.x)))
    };
    let top = if panel_y >= old_y {
        let gap = f64::from(old.y) + f64::from(old.h) - f64::from(panel.y) - f64::from(panel.h);
        r.bottom()
            .saturating_sub(scaled(gap))
            .saturating_sub(height)
    } else {
        i64::from(r.y).saturating_add(scaled(f64::from(panel.y) - f64::from(old.y)))
    };
    Rect {
        x: saturate_i32(left),
        y: saturate_i32(top),
        w: saturate_u32(width),
        h: saturate_u32(height),
    }
}

/// `rect` fitted inside one monitor's work area: the monitor at its centre,
/// else the one at the centre of `r`. Its size is first raised to the minimum
/// at that monitor's scale, then cut to the work area, which wins. With no
/// such monitor (an inconsistent list) `rect` is returned unchanged.
fn fit(rect: Rect, r: Rect, monitors: &[MonitorArea]) -> Rect {
    let at = |point: (f64, f64)| {
        monitors
            .iter()
            .find(|monitor| monitor.work.contains_point(point))
    };
    let Some(target) = at(rect.centre()).or_else(|| at(r.centre())) else {
        return rect;
    };
    let work = target.work;
    let min_w = saturate_u32(round(MIN_WIDTH * target.scale));
    let min_h = saturate_u32(round(MIN_HEIGHT * target.scale));
    let width = rect.w.max(min_w).min(work.w);
    let height = rect.h.max(min_h).min(work.h);
    let left = i64::from(rect.x)
        .max(i64::from(work.x))
        .min(work.right() - i64::from(width));
    let top = i64::from(rect.y)
        .max(i64::from(work.y))
        .min(work.bottom() - i64::from(height));
    Rect {
        x: saturate_i32(left),
        y: saturate_i32(top),
        w: width,
        h: height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    const fn area(x: i32, y: i32, w: u32, h: u32, scale: f64) -> MonitorArea {
        MonitorArea {
            work: rect(x, y, w, h),
            scale,
        }
    }

    const fn placed(x: i32, y: i32, w: u32, h: u32, from: PlacedFrom) -> Placed {
        Placed {
            rect: rect(x, y, w, h),
            from,
        }
    }

    /// A panel the user left at (1500,20) 420x900 against a 1920x1040 work
    /// area at 100 %.
    const MOVED: OverlayPlacement = OverlayPlacement {
        panel: rect(1500, 20, 420, 900),
        reference: rect(0, 0, 1920, 1040),
        scale: 1.0,
    };
    /// A panel the user left on a second monitor to the right.
    const ON_SECOND: OverlayPlacement = OverlayPlacement {
        panel: rect(2000, 0, 400, 1040),
        reference: rect(0, 0, 1920, 1040),
        scale: 1.0,
    };
    const TALLER: OverlayPlacement = OverlayPlacement {
        panel: rect(0, 0, 1000, 1400),
        reference: rect(0, 0, 2560, 1400),
        scale: 1.0,
    };
    const NO_SCALE: OverlayPlacement = OverlayPlacement {
        scale: 0.0,
        ..MOVED
    };
    const FULL_HD: Rect = rect(0, 0, 1920, 1040);
    const PRIMARY: MonitorArea = area(0, 0, 1920, 1040, 1.0);
    const SECOND: MonitorArea = area(1920, 0, 1920, 1040, 1.0);
    const LEFT: MonitorArea = area(-2560, 0, 2560, 1400, 1.0);

    /// Case, remembered record, reference rect, its scale, monitors, result.
    type PlaceCase = (
        &'static str,
        Option<OverlayPlacement>,
        Rect,
        f64,
        &'static [MonitorArea],
        Placed,
    );

    const PLACE_CASES: [PlaceCase; 15] = {
        use PlacedFrom::{Default, Moved, Restored};
        [
            (
                "1 default 100 %",
                None,
                FULL_HD,
                1.0,
                &[PRIMARY],
                placed(1520, 0, 400, 1040, Default),
            ),
            (
                "2 default 150 %",
                None,
                rect(0, 0, 3840, 2100),
                1.5,
                &[area(0, 0, 3840, 2100, 1.5)],
                placed(3240, 0, 600, 2100, Default),
            ),
            (
                "3 default 200 %",
                None,
                rect(0, 0, 3840, 2080),
                2.0,
                &[area(0, 0, 3840, 2080, 2.0)],
                placed(3040, 0, 800, 2080, Default),
            ),
            (
                "4 windowed game",
                None,
                rect(100, 100, 1280, 720),
                1.0,
                &[PRIMARY],
                placed(980, 100, 400, 720, Default),
            ),
            (
                "5 shorter than the minimum",
                None,
                rect(200, 200, 800, 300),
                1.0,
                &[PRIMARY],
                placed(600, 200, 400, 400, Default),
            ),
            (
                "6 exact restore",
                Some(MOVED),
                FULL_HD,
                1.0,
                &[PRIMARY],
                placed(1500, 20, 420, 900, Restored),
            ),
            (
                "7 on a second monitor",
                Some(ON_SECOND),
                FULL_HD,
                1.0,
                &[PRIMARY, SECOND],
                placed(2000, 0, 400, 1040, Restored),
            ),
            (
                "8 new resolution and scale",
                Some(MOVED),
                rect(0, 0, 2560, 1400),
                1.25,
                &[area(0, 0, 2560, 1400, 1.25)],
                placed(2035, 25, 525, 1125, Moved),
            ),
            (
                "9 scale changed, same reference",
                Some(MOVED),
                FULL_HD,
                1.25,
                &[area(0, 0, 1920, 1040, 1.25)],
                placed(1395, 0, 525, 1040, Moved),
            ),
            (
                "10 monitor left of the primary",
                None,
                LEFT.work,
                1.0,
                &[LEFT, PRIMARY],
                placed(-400, 0, 400, 1400, Default),
            ),
            (
                "11 re-anchor onto a negative origin",
                Some(MOVED),
                LEFT.work,
                1.0,
                &[LEFT, PRIMARY],
                placed(-420, 20, 420, 900, Moved),
            ),
            (
                "12 the saved monitor is gone",
                Some(ON_SECOND),
                FULL_HD,
                1.0,
                &[PRIMARY],
                placed(1520, 0, 400, 1040, Moved),
            ),
            (
                "13 taller than the work area",
                Some(TALLER),
                FULL_HD,
                1.0,
                &[PRIMARY],
                placed(0, 0, 1000, 1040, Moved),
            ),
            (
                "14 an empty record",
                Some(OverlayPlacement {
                    panel: rect(0, 0, 0, 0),
                    reference: rect(0, 0, 0, 0),
                    scale: 0.0,
                }),
                FULL_HD,
                1.0,
                &[PRIMARY],
                placed(1520, 0, 400, 1040, Default),
            ),
            (
                "14 a record with scale 0",
                Some(NO_SCALE),
                FULL_HD,
                1.0,
                &[PRIMARY],
                placed(1520, 0, 400, 1040, Default),
            ),
        ]
    };

    #[test]
    fn placement_table() {
        assert_eq!(
            PLACE_CASES[13].1,
            Some(OverlayPlacement::default()),
            "row 14 is the empty record"
        );
        let mut wrong = Vec::new();
        for (case, saved, r, r_scale, monitors, expected) in PLACE_CASES {
            let placed = place(saved.as_ref(), r, r_scale, monitors);
            println!("{case}: {placed:?}");
            if placed != expected {
                wrong.push(format!("{case}: {placed:?}, expected {expected:?}"));
            }
        }
        let n = PLACE_CASES.len();
        println!("checked {n} placement cases");
        assert!(n >= 14, "the table lost cases: {n}");
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn reference_cases() {
        let game_monitor = area(0, 0, 1920, 1040, 1.5);
        let cursor_monitor = area(1920, 0, 2560, 1400, 1.25);
        let cursor = Some((cursor_monitor.work, cursor_monitor.scale));
        let cases = [
            (
                "a windowed client inside the work area",
                Some(rect(100, 100, 1280, 720)),
                Some(game_monitor),
                Some(cursor_monitor),
                Some((rect(100, 100, 1280, 720), 1.5)),
            ),
            (
                "a borderless client over the taskbar",
                Some(rect(0, 0, 1920, 1080)),
                Some(area(0, 0, 1920, 1040, 1.0)),
                Some(cursor_monitor),
                Some((rect(0, 0, 1920, 1040), 1.0)),
            ),
            (
                "a client inside the taskbar strip",
                Some(rect(0, 1045, 100, 30)),
                Some(area(0, 0, 1920, 1040, 1.0)),
                Some(cursor_monitor),
                cursor,
            ),
            (
                "no client",
                None,
                Some(game_monitor),
                Some(cursor_monitor),
                cursor,
            ),
            (
                "no monitor at the client's centre",
                Some(rect(100, 100, 1280, 720)),
                None,
                Some(cursor_monitor),
                cursor,
            ),
            ("nothing known", None, None, None, None),
        ];
        let mut wrong = Vec::new();
        for (case, client, at_centre, at_cursor, expected) in cases {
            let found = reference(client, at_centre, at_cursor);
            println!("{case}: {found:?}");
            if found != expected {
                wrong.push(format!("{case}: {found:?}, expected {expected:?}"));
            }
        }
        let n = cases.len();
        println!("checked {n} reference cases");
        assert_eq!(n, 6, "the table lost cases");
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}
