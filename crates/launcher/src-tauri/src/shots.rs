//! Screenshots: the one capture pipeline, the sizes every sent image keeps,
//! and the screenshots held in memory until a request claims one.

use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::overlay::GameInfo;
use crate::placement::Rect;
use crate::region::{PixelRect, TranslateRegion};

/// How long a take may run before it is given up.
pub(crate) const SHOT_TIMEOUT: Duration = Duration::from_secs(5);

const TIMED_OUT: &str = "Taking the screenshot timed out.";
const CLOSED: &str = "The overlay was closed before the screenshot was ready.";
const GONE: &str = "The screenshot is no longer available; nothing was sent.";
const NOT_HELD: &str = "The screenshot is no longer available.";
pub(crate) const WINDOW_CLOSED: &str = "The linked window is no longer open.";
const OTHER_WINDOW: &str = "This command is only available in the Sage overlay.";
const CHAT_HAS_NO_REGION: &str = "A chat screenshot has no region.";
const REGION_UNUSABLE: &str = "The saved region cannot be applied -- edit or clear it.";

/// Which held screenshot a take fills and a request claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ShotSlot {
    Chat,
    Translate,
}

impl ShotSlot {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Translate => "translate",
        }
    }

    /// The event the slot's capture hotkey sends the overlay.
    pub(crate) const fn hotkey_event(self) -> &'static str {
        match self {
            Self::Chat => "quick-ask",
            Self::Translate => "translate-request",
        }
    }
}

/// What a capture hotkey tells the overlay with its action event.
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct HotkeyAction {
    /// The number of the take the hotkey started, if it started one; its
    /// result follows as `shot-held` with the same number.
    pub(crate) take: Option<u64>,
}

/// The result of a hotkey's take, sent to the overlay as `shot-held`: `shot`
/// when it is held, `error` when it failed, neither when a newer take into
/// the slot replaced it (the overlay then shows nothing for it).
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ShotHeld {
    slot: ShotSlot,
    take: u64,
    shot: Option<ShotInfo>,
    error: Option<String>,
}

/// What the overlay learns about a held screenshot: never its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShotInfo {
    id: u64,
    slot: ShotSlot,
    width: u32,
    height: u32,
    /// The bytes are a crop of the game's stored region.
    region: bool,
}

/// A screenshot held in memory from its take until it is claimed or dropped.
#[derive(Debug)]
pub(crate) struct HeldShot {
    id: u64,
    slot: ShotSlot,
    /// The window it shows, as (hwnd, pid).
    target: (i64, u32),
    /// What `shot_png` returns and a claim moves, byte for byte.
    png: Vec<u8>,
    width: u32,
    height: u32,
    /// The stored region the bytes were cropped to, when they are a crop.
    region: Option<TranslateRegion>,
}

/// The held screenshots: at most one per slot.
#[derive(Debug, Default)]
struct Held {
    /// The last id given out; ids start at 1.
    last_id: u64,
    /// Bumped by every clear, so a take that outlives it is dropped.
    epoch: u64,
    /// Per slot, the number of the newest take begun into it; numbers start
    /// at 1.
    chat_take: u64,
    translate_take: u64,
    chat: Option<HeldShot>,
    translate: Option<HeldShot>,
}

impl Held {
    const fn slot(&mut self, slot: ShotSlot) -> &mut Option<HeldShot> {
        match slot {
            ShotSlot::Chat => &mut self.chat,
            ShotSlot::Translate => &mut self.translate,
        }
    }

    const fn newest_take(&mut self, slot: ShotSlot) -> &mut u64 {
        match slot {
            ShotSlot::Chat => &mut self.chat_take,
            ShotSlot::Translate => &mut self.translate_take,
        }
    }
}

/// A take in progress: what its completion must still match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Take {
    slot: ShotSlot,
    /// The clear epoch when it began.
    epoch: u64,
    /// Its number among the takes into `slot`; the overlay matches the
    /// hotkey's events by it.
    number: u64,
}

/// The screenshots the overlay shows, held in memory only. The lock is never
/// held across an await, a capture or an encode.
#[derive(Debug, Default)]
pub(crate) struct ShotStore {
    held: Mutex<Held>,
}

impl ShotStore {
    /// Start a take into `slot`. It is the newest take into the slot until
    /// the next `begin` for it.
    pub(crate) fn begin(&self, slot: ShotSlot) -> Take {
        let mut held = self.held.lock();
        let newest = held.newest_take(slot);
        *newest += 1;
        let number = *newest;
        let epoch = held.epoch;
        drop(held);
        Take {
            slot,
            epoch,
            number,
        }
    }

    /// Whether `take` is still the newest take begun into its slot.
    pub(crate) fn is_newest(&self, take: Take) -> bool {
        *self.held.lock().newest_take(take.slot) == take.number
    }

    /// Hold `prepared` in the take's slot under a new id, replacing what the
    /// slot held. `Ok(None)`, dropping `prepared`, when a newer take into the
    /// slot began since: the overlay shows that one. An error when a clear
    /// happened since `take` began. `region` is the stored region it was
    /// cropped to.
    pub(crate) fn complete(
        &self,
        take: Take,
        target: (i64, u32),
        region: Option<TranslateRegion>,
        prepared: Prepared,
    ) -> Result<Option<ShotInfo>, String> {
        let mut held = self.held.lock();
        if held.epoch != take.epoch {
            return Err(CLOSED.to_owned());
        }
        if *held.newest_take(take.slot) != take.number {
            return Ok(None);
        }
        held.last_id += 1;
        let id = held.last_id;
        let stored = held.slot(take.slot).insert(HeldShot {
            id,
            slot: take.slot,
            target,
            png: prepared.png,
            width: prepared.width,
            height: prepared.height,
            region,
        });
        let info = ShotInfo {
            id: stored.id,
            slot: stored.slot,
            width: stored.width,
            height: stored.height,
            region: stored.region.is_some(),
        };
        drop(held);
        Ok(Some(info))
    }

    /// Drop every held screenshot and end every take in progress. Returns how
    /// many were dropped.
    pub(crate) fn clear(&self) -> usize {
        let mut guard = self.held.lock();
        let held = &mut *guard;
        held.epoch += 1;
        let dropped = [held.chat.take(), held.translate.take()]
            .into_iter()
            .flatten()
            .count();
        drop(guard);
        if dropped > 0 {
            tracing::info!("Held screenshots dropped: {dropped}");
        }
        dropped
    }

    /// Drop the screenshot `id`; an id not held is a no-op.
    pub(crate) fn drop_id(&self, id: u64) {
        let mut guard = self.held.lock();
        let held = &mut *guard;
        for slot in [&mut held.chat, &mut held.translate] {
            slot.take_if(|shot| shot.id == id);
        }
        drop(guard);
    }

    /// The PNG of the held screenshot `id`.
    pub(crate) fn png(&self, id: u64) -> Option<Vec<u8>> {
        let held = self.held.lock();
        let png = [&held.chat, &held.translate]
            .into_iter()
            .flatten()
            .find(|shot| shot.id == id)
            .map(|shot| shot.png.clone());
        drop(held);
        png
    }

    /// Decide once whether the screenshot `requested` goes with a request, and
    /// on yes move its bytes out of `slot`. A refusal changes nothing.
    pub(crate) fn claim_for_request(
        &self,
        slot: ShotSlot,
        requested: Option<u64>,
        target: Option<(i64, u32)>,
        region: Option<&TranslateRegion>,
        accepts_image: bool,
    ) -> Result<Option<Vec<u8>>, ShotRefusal> {
        let mut held = self.held.lock();
        let slot = held.slot(slot);
        let claimed = if claim_shot(slot.as_ref(), requested, target, region, accepts_image)? {
            slot.take().map(|shot| shot.png)
        } else {
            None
        };
        drop(held);
        Ok(claimed)
    }
}

/// Why a request may not take the screenshot it named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShotRefusal {
    /// The provider takes no image.
    NotAccepted,
    /// The screenshot is no longer held.
    Gone,
    /// It shows a window that is not the linked game now.
    OtherTarget,
    /// It was cropped to a region other than the one stored now.
    RegionChanged,
}

impl ShotRefusal {
    /// What the overlay shows for the refusal; `provider` names the provider
    /// the request was for.
    pub(crate) fn message(self, provider: &str) -> String {
        match self {
            Self::NotAccepted => {
                format!("{provider} does not take screenshots. Remove the screenshot to send.")
            }
            Self::Gone | Self::OtherTarget | Self::RegionChanged => GONE.to_owned(),
        }
    }
}

/// Whether the held screenshot goes with a request that names `requested`:
/// `Ok(false)` for a text-only request, `Ok(true)` when the screenshot shown
/// is still held, of the linked `target`, cropped to the stored `region` (a
/// whole frame when none is stored), and the provider takes images.
pub(crate) fn claim_shot(
    held: Option<&HeldShot>,
    requested: Option<u64>,
    target: Option<(i64, u32)>,
    region: Option<&TranslateRegion>,
    accepts_image: bool,
) -> Result<bool, ShotRefusal> {
    let Some(requested) = requested else {
        return Ok(false);
    };
    if !accepts_image {
        return Err(ShotRefusal::NotAccepted);
    }
    let held = held
        .filter(|shot| shot.id == requested)
        .ok_or(ShotRefusal::Gone)?;
    if target != Some(held.target) {
        return Err(ShotRefusal::OtherTarget);
    }
    if held.region.as_ref() != region {
        return Err(ShotRefusal::RegionChanged);
    }
    Ok(true)
}

/// The window a take may capture, as (hwnd, pid): only a linked target whose
/// exe is known.
pub(crate) fn capture_target(target: Option<&GameInfo>) -> Option<(i64, u32)> {
    target
        .filter(|game| game.linked && !game.exe.is_empty())
        .map(|game| (game.hwnd, game.pid))
}

/// Why a take or a translation found no window to use, given the target
/// `stored` before it was checked to be alive.
pub(crate) fn missing_target(stored: Option<&GameInfo>) -> &'static str {
    if capture_target(stored).is_some() {
        WINDOW_CLOSED
    } else {
        crate::overlay::NO_LINKED_GAME
    }
}

/// Refuse `command` unless the overlay window called it.
pub(crate) fn overlay_only(window: &WebviewWindow, command: &str) -> Result<(), String> {
    if window.label() == "overlay" {
        return Ok(());
    }
    tracing::warn!("{command} refused: called from another window");
    Err(OTHER_WINDOW.to_owned())
}

/// What started a take, for its log line.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Via {
    Overlay,
    Hotkey,
}

impl Via {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Overlay => "overlay",
            Self::Hotkey => "hotkey",
        }
    }
}

/// The one log line of a take: sizes and timings, never a name or a byte.
pub(crate) fn shot_log_line(
    info: &ShotInfo,
    via: Via,
    bytes: usize,
    from: (u32, u32),
    frame_ms: u128,
    total_ms: u128,
) -> String {
    format!(
        "Shot {}: {} via {}, {}x{} from {}x{}, {bytes} bytes, region: {}, frame {frame_ms} ms, total {total_ms} ms",
        info.id,
        info.slot.as_str(),
        via.as_str(),
        info.width,
        info.height,
        from.0,
        from.1,
        if info.region { "yes" } else { "no" },
    )
}

/// The log line of a take a newer take into its slot replaced: its frame is
/// dropped and the overlay shows nothing for it.
fn log_replaced(slot: ShotSlot, via: Via) {
    tracing::debug!(
        "Shot replaced: {} via {}, a newer take began",
        slot.as_str(),
        via.as_str()
    );
}

/// Capture the linked game into `slot` and hold it for the overlay to show.
/// Nothing is sent: a request claims the screenshot later, by its id. `None`
/// when a newer take into the slot replaced this one.
#[tauri::command]
pub(crate) async fn take_shot(
    app: AppHandle,
    window: WebviewWindow,
    slot: ShotSlot,
    edit_region: bool,
) -> Result<Option<ShotInfo>, String> {
    overlay_only(&window, "take_shot")?;
    let taken = take(&app, slot, edit_region).await;
    if let Err(error) = &taken {
        tracing::warn!("Shot failed: {} via overlay: {error}", slot.as_str());
    }
    taken
}

/// `take_shot` once the calling window is checked.
async fn take(
    app: &AppHandle,
    slot: ShotSlot,
    edit_region: bool,
) -> Result<Option<ShotInfo>, String> {
    if slot == ShotSlot::Chat && edit_region {
        return Err(CHAT_HAS_NO_REGION.to_owned());
    }
    let stored = crate::overlay::stored_game(app);
    let linked = crate::overlay::linked_game(app);
    let target = capture_target(linked.as_ref())
        .ok_or_else(|| missing_target(stored.as_ref()).to_owned())?;
    // A translate screenshot is cut to the game's stored region; one taken to
    // edit the region shows the whole frame.
    let region = if slot == ShotSlot::Translate && !edit_region {
        crate::region::linked_region(app, linked.as_ref())
    } else {
        None
    };
    let store = app.state::<ShotStore>();
    let take = store.begin(slot);
    let started = Instant::now();
    let (hwnd, pid) = target;
    let job = tokio::task::spawn_blocking(move || {
        let frame = grab_frame(hwnd, pid)?;
        let frame_ms = started.elapsed().as_millis();
        finish_frame(frame, region.as_ref()).map(|(prepared, from)| (prepared, from, frame_ms))
    });
    let (prepared, from, frame_ms) = settle(tokio::time::timeout(SHOT_TIMEOUT, job).await)?;
    let bytes = prepared.png.len();
    let Some(info) = store.complete(take, target, region, prepared)? else {
        log_replaced(slot, Via::Overlay);
        return Ok(None);
    };
    let total_ms = started.elapsed().as_millis();
    let line = shot_log_line(&info, Via::Overlay, bytes, from, frame_ms, total_ms);
    tracing::info!("{line}");
    Ok(Some(info))
}

/// The result of a capture job run under a timeout. On a timeout the job runs
/// on; its late result is dropped with it.
fn settle<T>(
    outcome: Result<Result<Result<T, String>, tokio::task::JoinError>, tokio::time::error::Elapsed>,
) -> Result<T, String> {
    outcome
        .map_err(|_elapsed| TIMED_OUT.to_owned())?
        .map_err(|error| format!("capture task failed: {error}"))?
}

/// Take the linked game `target` into `slot` for a capture hotkey. The frame
/// is grabbed while the game still has focus; then `shown`, on the main
/// thread, shows the overlay and sends the hotkey's action event carrying the
/// take's number. Only when it returns `true` is the frame sized, held and
/// reported as `shot-held`. A take that a newer one into the slot replaced
/// during its grab sends no action event: the overlay then sees the takes in
/// the order the store holds them.
pub(crate) fn take_for_hotkey<F>(
    app: AppHandle,
    slot: ShotSlot,
    target: (i64, u32),
    region: Option<TranslateRegion>,
    shown: F,
) where
    F: FnOnce(&AppHandle, u64) -> bool + Send + 'static,
{
    use tauri::Emitter;

    let take = app.state::<ShotStore>().begin(slot);
    tauri::async_runtime::spawn(async move {
        let started = Instant::now();
        let (hwnd, pid) = target;
        let grab = tokio::task::spawn_blocking(move || grab_frame(hwnd, pid));
        let frame = settle(tokio::time::timeout(SHOT_TIMEOUT, grab).await);
        let frame_ms = started.elapsed().as_millis();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = app.clone();
        let dispatched = app.run_on_main_thread(move || {
            let newest = handle.state::<ShotStore>().is_newest(take);
            if !newest {
                log_replaced(slot, Via::Hotkey);
            }
            if tx.send(newest && shown(&handle, take.number)).is_err() {
                tracing::debug!("Hotkey take ended before the overlay was shown");
            }
        });
        if let Err(error) = dispatched {
            tracing::warn!("Shot failed: {} via hotkey: {error}", slot.as_str());
            app.state::<ShotStore>().clear();
            return;
        }
        if !rx.await.unwrap_or(false) {
            return;
        }
        let finished = match frame {
            Ok(frame) => {
                let rest = SHOT_TIMEOUT.saturating_sub(started.elapsed());
                let job = tokio::task::spawn_blocking(move || finish_frame(frame, region.as_ref()));
                settle(tokio::time::timeout(rest, job).await)
            }
            Err(error) => Err(error),
        };
        let held = finished.and_then(|(prepared, from)| {
            let bytes = prepared.png.len();
            let Some(info) = app
                .state::<ShotStore>()
                .complete(take, target, region, prepared)?
            else {
                log_replaced(slot, Via::Hotkey);
                return Ok(None);
            };
            let total_ms = started.elapsed().as_millis();
            let line = shot_log_line(&info, Via::Hotkey, bytes, from, frame_ms, total_ms);
            tracing::info!("{line}");
            Ok(Some(info))
        });
        let report = match held {
            Ok(info) => ShotHeld {
                slot,
                take: take.number,
                shot: info,
                error: None,
            },
            Err(error) => {
                tracing::warn!("Shot failed: {} via hotkey: {error}", slot.as_str());
                ShotHeld {
                    slot,
                    take: take.number,
                    shot: None,
                    error: Some(error),
                }
            }
        };
        crate::util::log_if_err(
            "emit shot-held",
            app.emit_to("overlay", "shot-held", report),
        );
    });
}

/// The held screenshot `id` as PNG bytes, sent raw (an `ArrayBuffer` in JS).
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn shot_png(
    window: WebviewWindow,
    store: State<'_, ShotStore>,
    id: u64,
) -> Result<tauri::ipc::Response, String> {
    overlay_only(&window, "shot_png")?;
    store
        .png(id)
        .map(tauri::ipc::Response::new)
        .ok_or_else(|| NOT_HELD.to_owned())
}

/// Drop the held screenshot `id` (Remove, New chat, a preview that failed).
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a Tauri command receives its arguments by value"
)]
pub(crate) fn drop_shot(window: WebviewWindow, store: State<'_, ShotStore>, id: u64) {
    if overlay_only(&window, "drop_shot").is_ok() {
        store.drop_id(id);
    }
}

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
#[derive(Clone, Debug)]
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

/// Size and encode a captured frame for sending, cut to `region` first when
/// one is stored. Returns the image and the size of the source it was made
/// from. A region that cannot be applied is an error: the whole frame would
/// show more than the user kept.
pub(crate) fn finish_frame(
    frame: Frame,
    region: Option<&TranslateRegion>,
) -> Result<(Prepared, (u32, u32)), String> {
    let source = source(frame);
    let from = (source.width, source.height);
    let prepared = match region {
        None => prepare(&source.rgba, source.width, source.height)?,
        Some(region) => {
            let crop = crate::region::framed(&source, Some(region)).map_err(|error| {
                tracing::debug!("Stored region not applied: {error:?}");
                REGION_UNUSABLE.to_owned()
            })?;
            prepare(&crop.rgba, crop.width, crop.height)?
        }
    };
    Ok((prepared, from))
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
pub(crate) const fn floor_px(value: f64) -> u32 {
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
    use super::{Frame, ShotSlot, ShotStore, client_box, fit_within, prepare, source};
    use super::{HeldShot, HotkeyAction, Prepared, ShotHeld, capture_target, claim_shot};
    use super::{NOT_HELD, WINDOW_CLOSED, missing_target};
    use super::{SHOT_MAX_BYTES, SHOT_MAX_H, SHOT_MAX_W, SOURCE_MAX_H, SOURCE_MAX_W};
    use super::{ShotInfo, ShotRefusal, Take};
    use crate::overlay::GameInfo;
    use crate::placement::Rect;
    use crate::region::PixelRect;
    use std::path::Path;

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
        let grab = concat!("grab", "_frame(");
        let grabs: Vec<(String, usize)> = crate::util::count_in_sources_by_file(grab)
            .into_iter()
            .filter(|(file, count)| *count > 0 && file != Path::new("shots.rs"))
            .map(|(file, count)| (file.display().to_string(), count))
            .collect();
        println!("{grab} outside shots.rs: {grabs:?}");
        assert!(grabs.is_empty(), "only a take captures a frame");
    }

    #[test]
    fn every_sent_image_is_claimed() {
        let claim = concat!("claim_for", "_request(");
        let rows = crate::util::count_in_sources_by_file(claim);
        println!("scanned {} files", rows.len());
        let claims: Vec<(&Path, usize)> = rows
            .iter()
            .filter(|(file, count)| *count > 0 && file != Path::new("shots.rs"))
            .map(|(file, count)| (file.as_path(), *count))
            .collect();
        println!("{claim} outside shots.rs: {claims:?}");
        assert_eq!(
            claims.iter().map(|(_, count)| count).sum::<usize>(),
            2,
            "a chat claim and a translate claim"
        );
        assert!(
            claims.iter().all(|(file, _)| file.starts_with("ai")),
            "every claim is in the request code"
        );
        assert!(
            claims
                .iter()
                .any(|(file, _)| *file == Path::new("ai").join("mod.rs")),
            "a claim in ai/mod.rs"
        );
        for needle in [
            concat!("capture", "_png("),
            concat!("attach", "_screenshot"),
        ] {
            let found: Vec<_> = crate::util::count_in_sources_by_file(needle)
                .into_iter()
                .filter(|(_, count)| *count > 0)
                .collect();
            println!("{needle}: {found:?}");
            assert!(found.is_empty(), "{needle} sends an unseen screenshot");
        }
        let flag = concat!("attach", "Screenshot");
        let scan = crate::util::count_in_frontend(&[flag]);
        println!("{} frontend files scanned", scan.files.len());
        println!("{flag}: {:?}", scan.hits[0]);
        assert!(scan.hits[0].is_empty(), "{flag} sends an unseen screenshot");
    }

    #[test]
    fn no_dispatch_from_hotkey_or_editing() {
        let hotkey = ["overlay.rs", "shots.rs", "region.rs"];
        let request_kind = concat!("Request", "Kind");
        let ai_path = concat!("ai", "::");
        let checks: [(&str, &[&str]); 6] = [
            (request_kind, &hotkey),
            (ai_path, &["shots.rs", "region.rs"]),
            (concat!("admit", "_chat("), &["overlay.rs"]),
            (concat!("translate", "::start"), &["overlay.rs"]),
            (concat!("ask", "_sage"), &["overlay.rs"]),
            (concat!("translate", "_screen"), &["overlay.rs"]),
        ];
        for (needle, files) in checks {
            let rows = crate::util::count_in_sources_by_file(needle);
            for file in hotkey {
                assert!(
                    rows.iter().any(|(path, _)| path == Path::new(file)),
                    "{file} was not scanned"
                );
            }
            for file in files {
                let count = rows
                    .iter()
                    .find(|(path, _)| path == Path::new(file))
                    .map_or(0, |(_, count)| *count);
                println!("{needle} in {file}: {count}");
                assert_eq!(count, 0, "{needle} in {file} starts a request");
            }
        }
    }

    #[test]
    fn hotkey_events_name_their_slot() {
        let cases = [
            (ShotSlot::Chat, "quick-ask"),
            (ShotSlot::Translate, "translate-request"),
        ];
        let listened: Vec<String> = cases
            .iter()
            .map(|(_, event)| format!("listen<HotkeyAction>('{event}'"))
            .collect();
        let needles: Vec<&str> = listened.iter().map(String::as_str).collect();
        let scan = crate::util::count_in_frontend(&needles);
        println!("{} frontend files scanned", scan.files.len());
        for ((slot, event), hits) in cases.into_iter().zip(&scan.hits) {
            println!(
                "{slot:?}: {} (page listeners: {hits:?})",
                slot.hotkey_event()
            );
            assert_eq!(slot.hotkey_event(), event, "{slot:?}");
            assert_eq!(hits.len(), 1, "one page listener for {event}");
        }
    }

    const TARGET: (i64, u32) = (42, 7);

    /// A prepared image whose bytes are all `fill`.
    fn prepared(fill: u8) -> Prepared {
        Prepared {
            png: vec![fill; 4],
            width: 2,
            height: 1,
        }
    }

    #[test]
    fn shot_store_ids_slots_epoch() {
        let store = ShotStore::default();
        let hold = |take: Take, fill: u8| {
            store
                .complete(take, TARGET, None, prepared(fill))
                .unwrap()
                .expect("the newest take into its slot is held")
        };
        let first = hold(store.begin(ShotSlot::Chat), 1);
        let second = hold(store.begin(ShotSlot::Chat), 2);
        println!("chat: {first:?} then {second:?}");
        assert_eq!(first.id, 1, "ids start at 1");
        assert!(second.id > first.id, "every completion gets a new id");
        assert_eq!(store.png(first.id), None, "a replaced shot is gone");
        assert_eq!(store.png(second.id), Some(vec![2; 4]));
        assert_eq!(
            second,
            ShotInfo {
                id: second.id,
                slot: ShotSlot::Chat,
                width: 2,
                height: 1,
                region: false,
            }
        );

        let before_clear = store.begin(ShotSlot::Chat);
        let third = hold(store.begin(ShotSlot::Translate), 3);
        assert!(third.id > second.id);
        store.drop_id(999);
        assert_eq!(store.png(999), None);
        assert!(
            store.png(second.id).is_some() && store.png(third.id).is_some(),
            "dropping an id not held drops nothing"
        );

        assert_eq!(store.clear(), 2, "both slots were full");
        assert_eq!((store.png(second.id), store.png(third.id)), (None, None));
        let stale = store.complete(before_clear, TARGET, None, prepared(4));
        println!("completion with the epoch before the clear: {stale:?}");
        assert!(stale.is_err(), "a take that outlived a clear is dropped");
        let fresh = store.begin(ShotSlot::Chat);
        assert_ne!(
            fresh.epoch, before_clear.epoch,
            "a clear starts a new epoch"
        );
        let next = hold(fresh, 5);
        assert_eq!(next.id, third.id + 1, "the stale completion took no id");
        assert_eq!(store.clear(), 1, "the stale completion held nothing");
    }

    #[test]
    fn newest_take_into_a_slot_wins() {
        let store = ShotStore::default();
        let older = store.begin(ShotSlot::Chat);
        let newer = store.begin(ShotSlot::Chat);
        // A take into the other slot replaces nothing here.
        let other = store.begin(ShotSlot::Translate);
        assert!(!store.is_newest(older) && store.is_newest(newer));
        let held = store
            .complete(newer, TARGET, None, prepared(2))
            .unwrap()
            .expect("the newer take is held");
        let late = store.complete(older, TARGET, None, prepared(1));
        println!("newer take held {held:?}; the older one finishing last: {late:?}");
        assert_eq!(
            store.png(held.id),
            Some(vec![2; 4]),
            "the newer take is still held"
        );
        assert_eq!(late, Ok(None), "the older take is refused without an error");
        let translate = store
            .complete(other, TARGET, None, prepared(3))
            .unwrap()
            .expect("the translate take is the newest in its slot");
        assert_eq!(translate.id, held.id + 1, "the refused take took no id");
        assert_eq!(store.clear(), 2, "the refused take held nothing");
    }

    fn held(id: u64) -> HeldShot {
        HeldShot {
            id,
            slot: ShotSlot::Chat,
            target: TARGET,
            png: vec![9; 3],
            width: 2,
            height: 1,
            region: None,
        }
    }

    /// A stored region, read the way the state file is.
    fn band() -> crate::region::TranslateRegion {
        serde_json::from_str(r#"{"x":0,"y":0.5,"w":1,"h":0.5}"#).unwrap()
    }

    #[test]
    fn claim_table() {
        use ShotRefusal::{Gone, NotAccepted, OtherTarget};
        let shot = held(5);
        let other = Some((43, 7));
        // Each row: case, held, requested, target, accepts_image, expected.
        let rows = [
            (
                "text only",
                Some(&shot),
                None,
                Some(TARGET),
                true,
                Ok(false),
            ),
            (
                "text only, nothing held",
                None,
                None,
                None,
                false,
                Ok(false),
            ),
            (
                "provider takes no image",
                Some(&shot),
                Some(5),
                Some(TARGET),
                false,
                Err(NotAccepted),
            ),
            (
                "takes no image, nothing held",
                None,
                Some(5),
                None,
                false,
                Err(NotAccepted),
            ),
            ("nothing held", None, Some(5), Some(TARGET), true, Err(Gone)),
            (
                "another id held",
                Some(&shot),
                Some(4),
                Some(TARGET),
                true,
                Err(Gone),
            ),
            (
                "another id, another target",
                Some(&shot),
                Some(4),
                other,
                true,
                Err(Gone),
            ),
            (
                "no linked target",
                Some(&shot),
                Some(5),
                None,
                true,
                Err(OtherTarget),
            ),
            (
                "another target",
                Some(&shot),
                Some(5),
                other,
                true,
                Err(OtherTarget),
            ),
            (
                "the shot shown",
                Some(&shot),
                Some(5),
                Some(TARGET),
                true,
                Ok(true),
            ),
        ];
        for (case, held, requested, target, accepts, expected) in rows {
            let got = claim_shot(held, requested, target, None, accepts);
            println!("{case}: {got:?}");
            assert_eq!(got, expected, "{case}");
        }
        claims_by_region(&shot, other);
        claims_through_the_store(other);
    }

    /// `claim_table`'s region rows: the region the screenshot was cropped to
    /// must be the one stored now, checked after the target.
    fn claims_by_region(shot: &HeldShot, other: Option<(i64, u32)>) {
        use ShotRefusal::{OtherTarget, RegionChanged};
        let band = band();
        let cropped = HeldShot {
            region: Some(band),
            ..held(5)
        };
        // Each row: case, held, target, region, expected.
        let region_rows = [
            (
                "a region stored since the take",
                shot,
                Some(TARGET),
                Some(&band),
                Err(RegionChanged),
            ),
            (
                "the region cleared since the take",
                &cropped,
                Some(TARGET),
                None,
                Err(RegionChanged),
            ),
            (
                "another target and another region",
                &cropped,
                other,
                None,
                Err(OtherTarget),
            ),
            (
                "the crop of the stored region",
                &cropped,
                Some(TARGET),
                Some(&band),
                Ok(true),
            ),
        ];
        for (case, held, target, region, expected) in region_rows {
            let got = claim_shot(Some(held), Some(5), target, region, true);
            println!("{case}: {got:?}");
            assert_eq!(got, expected, "{case}");
        }
    }

    /// `claim_table` through `ShotStore`: only `Ok(true)` moves the bytes out
    /// of the slot, and a claim reads only its own slot.
    fn claims_through_the_store(other: Option<(i64, u32)>) {
        let band = band();
        for (case, requested, target, region, accepts) in [
            ("text only", None, Some(TARGET), None, true),
            (
                "provider takes no image",
                Some(1),
                Some(TARGET),
                None,
                false,
            ),
            ("another id", Some(2), Some(TARGET), None, true),
            ("another target", Some(1), other, None, true),
            ("another region", Some(1), Some(TARGET), Some(&band), true),
            ("the shot shown", Some(1), Some(TARGET), None, true),
        ] {
            let store = ShotStore::default();
            let info = store
                .complete(store.begin(ShotSlot::Chat), TARGET, None, prepared(7))
                .unwrap()
                .unwrap();
            assert_eq!(info.id, 1);
            let claimed =
                store.claim_for_request(ShotSlot::Chat, requested, target, region, accepts);
            let left = store.png(info.id);
            println!("store, {case}: {claimed:?}, still held: {}", left.is_some());
            if case == "the shot shown" {
                assert_eq!(claimed, Ok(Some(vec![7; 4])), "{case}: the held bytes");
                assert_eq!(left, None, "{case}: claimed bytes leave the slot");
            } else {
                assert!(matches!(claimed, Ok(None) | Err(_)), "{case}: {claimed:?}");
                assert_eq!(left, Some(vec![7; 4]), "{case}: the store is unchanged");
            }
        }

        // A claim reads only its own slot.
        let store = ShotStore::default();
        let info = store
            .complete(store.begin(ShotSlot::Chat), TARGET, None, prepared(7))
            .unwrap()
            .unwrap();
        let wrong_slot =
            store.claim_for_request(ShotSlot::Translate, Some(info.id), Some(TARGET), None, true);
        assert_eq!(wrong_slot, Err(ShotRefusal::Gone));
        assert_eq!(store.png(info.id), Some(vec![7; 4]));

        // A cropped translate screenshot goes with the region it was cut to.
        let info = store
            .complete(
                store.begin(ShotSlot::Translate),
                TARGET,
                Some(band),
                prepared(8),
            )
            .unwrap()
            .unwrap();
        assert!(info.region, "the overlay learns the screenshot is a crop");
        let claimed = store.claim_for_request(
            ShotSlot::Translate,
            Some(info.id),
            Some(TARGET),
            Some(&band),
            true,
        );
        assert_eq!(claimed, Ok(Some(vec![8; 4])), "the cropped bytes");
    }

    #[test]
    fn capture_target_needs_a_linked_identified_window() {
        let game = |linked: bool, exe: &str| GameInfo {
            hwnd: 42,
            pid: 7,
            exe: exe.to_owned(),
            linked,
            ..GameInfo::default()
        };
        let cases = [
            ("no target", None, None),
            ("unlinked", Some(game(false, r"C:\Games\Foo\foo.exe")), None),
            ("linked, exe unknown", Some(game(true, "")), None),
            (
                "linked",
                Some(game(true, r"C:\Games\Foo\foo.exe")),
                Some(TARGET),
            ),
        ];
        for (case, target, expected) in cases {
            let got = capture_target(target.as_ref());
            println!("{case}: {got:?}");
            assert_eq!(got, expected, "{case}");
        }
    }

    #[test]
    fn missing_target_tells_a_closed_window_from_no_link() {
        let game = |linked: bool| GameInfo {
            hwnd: 42,
            pid: 7,
            exe: r"C:\Games\Foo\foo.exe".to_owned(),
            linked,
            ..GameInfo::default()
        };
        let cases = [
            ("nothing stored", None, crate::overlay::NO_LINKED_GAME),
            (
                "stored, never linked",
                Some(game(false)),
                crate::overlay::NO_LINKED_GAME,
            ),
            ("linked window gone", Some(game(true)), WINDOW_CLOSED),
        ];
        for (case, stored, expected) in cases {
            let got = missing_target(stored.as_ref());
            println!("{case}: {got}");
            assert_eq!(got, expected, "{case}");
        }
        assert_ne!(WINDOW_CLOSED, crate::overlay::NO_LINKED_GAME);
    }

    #[test]
    fn unavailable_screenshot_copy_offers_no_impossible_step() {
        use ShotRefusal::{Gone, OtherTarget, RegionChanged};
        for refusal in [Gone, OtherTarget, RegionChanged] {
            let message = refusal.message("Gemini");
            println!("{refusal:?}: {message}");
            assert!(
                message.ends_with("nothing was sent."),
                "{refusal:?}: {message}"
            );
            assert!(
                !message.to_lowercase().contains("remove"),
                "{refusal:?}: {message}"
            );
            assert!(
                !message.to_lowercase().contains("take it"),
                "{refusal:?}: {message}"
            );
        }
        println!("not held: {NOT_HELD}");
        assert!(!NOT_HELD.to_lowercase().contains("remove"), "{NOT_HELD}");
        assert!(!NOT_HELD.to_lowercase().contains("take it"), "{NOT_HELD}");
    }

    #[test]
    fn shot_types_mirror_typescript() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/utils/shots.ts"
        ));

        let info = ShotInfo {
            id: 3,
            slot: ShotSlot::Translate,
            width: 640,
            height: 360,
            region: true,
        };
        let typescript = crate::util::ts_fields(source, "interface ShotInfo {");
        let rust = crate::util::json_fields(&serde_json::to_value(&info).unwrap());
        println!(
            "ShotInfo: {} fields from Rust, {} in TypeScript",
            rust.len(),
            typescript.len()
        );
        assert!(!rust.is_empty() && !typescript.is_empty());
        // ShotInfo has no Option field, so the same list stands for both.
        let mismatches = crate::util::mirror_mismatches(&typescript, &rust, &rust, source);
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        let full = serde_json::to_value(HotkeyAction { take: Some(3) }).unwrap();
        let empty = serde_json::to_value(HotkeyAction { take: None }).unwrap();
        let (full, empty) = (
            crate::util::json_fields(&full),
            crate::util::json_fields(&empty),
        );
        let typescript = crate::util::ts_fields(source, "interface HotkeyAction {");
        println!("HotkeyAction: full {full:?}, empty {empty:?}, TypeScript {typescript:?}");
        assert!(!full.is_empty() && !typescript.is_empty());
        let mismatches = crate::util::mirror_mismatches(&typescript, &full, &empty, source);
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        let full = ShotHeld {
            slot: ShotSlot::Chat,
            take: 3,
            shot: Some(info),
            error: Some("Taking the screenshot timed out.".to_owned()),
        };
        let empty = ShotHeld {
            slot: ShotSlot::Translate,
            take: 3,
            shot: None,
            error: None,
        };
        let full = serde_json::to_value(&full).unwrap();
        let empty = serde_json::to_value(&empty).unwrap();
        let (full, empty) = (
            crate::util::json_fields(&full),
            crate::util::json_fields(&empty),
        );
        let typescript = crate::util::ts_fields(source, "interface ShotHeld {");
        println!("ShotHeld: full {full:?}, empty {empty:?}, TypeScript {typescript:?}");
        assert!(!full.is_empty() && !typescript.is_empty());
        let mismatches = crate::util::mirror_mismatches(&typescript, &full, &empty, source);
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        let slots: Vec<String> = [ShotSlot::Chat, ShotSlot::Translate]
            .into_iter()
            .map(|slot| {
                // A new variant fails to compile here; add it to the list too.
                match slot {
                    ShotSlot::Chat | ShotSlot::Translate => {}
                }
                serde_json::to_value(slot)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let names: Vec<&str> = slots.iter().map(String::as_str).collect();
        println!("ShotSlot: {} variants compared: {names:?}", names.len());
        assert_ne!(names.len(), 0);
        let mismatches = crate::util::alias_mismatches(source, "ShotSlot", &names);
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }
}
