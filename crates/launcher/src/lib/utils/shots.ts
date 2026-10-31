// A screenshot the overlay shows before anything is sent. Each slot (chat,
// translate) moves through these states; only a shot the page has drawn can
// go with a request.

/** Which held screenshot a take fills (Rust `ShotSlot`). */
export type ShotSlot = 'chat' | 'translate';

/** A held screenshot as `take_shot` reports it (Rust `ShotInfo`); never its bytes. */
export interface ShotInfo {
  id: number;
  slot: ShotSlot;
  width: number;
  height: number;
  region: boolean;
}

/** The payload of `quick-ask` and `translate-request` (Rust `HotkeyAction`). */
export interface HotkeyAction {
  // The number of the take the hotkey started, if any; its result arrives as
  // `shot-held` with the same number.
  take: number | null;
}

/**
 * The result of a hotkey's take, as `shot-held` reports it (Rust `ShotHeld`):
 * `shot` when held, `error` when it failed, neither when a newer take into the
 * slot replaced it.
 */
export interface ShotHeld {
  slot: ShotSlot;
  take: number;
  shot: ShotInfo | null;
  error: string | null;
}

/** The hotkey take a slot waits for: shown as `seq`, numbered `take` by Rust. */
export interface HotkeyTake {
  seq: number;
  take: number;
}

/** Where one slot's screenshot is, as the page sees it. */
export type ShotState =
  | { kind: 'none' }
  | { kind: 'taking'; seq: number }
  | { kind: 'fetched'; info: ShotInfo }
  | { kind: 'shown'; info: ShotInfo }
  | { kind: 'failed'; message: string }
  | { kind: 'sent'; info: ShotInfo };

/** No screenshot: the start, and every reset. */
export const NO_SHOT: ShotState = { kind: 'none' };

/** A take started; `seq` tells its result apart from an older take's. */
export function taking(seq: number): ShotState {
  return { kind: 'taking', seq };
}

/**
 * The take `seq` returned `info`. Anything but that take in progress returns
 * `state` itself: the caller then drops `info.id`, which nothing will show.
 */
export function received(state: ShotState, info: ShotInfo, seq: number): ShotState {
  return state.kind === 'taking' && state.seq === seq ? { kind: 'fetched', info } : state;
}

/**
 * A newer take replaced the take `seq` before anything was held for it: the
 * slot shows nothing, and no message. Anything but that take in progress is
 * unchanged.
 */
export function replaced(state: ShotState, seq: number): ShotState {
  return state.kind === 'taking' && state.seq === seq ? NO_SHOT : state;
}

/**
 * `held` reports a hotkey's take: `received`, `takeFailed` or `replaced` for
 * the take the slot waits for. A report of any other take returns `state`
 * itself, as does a stale one; the caller then drops the shot.
 */
export function heldShot(state: ShotState, held: ShotHeld, hotkey: HotkeyTake): ShotState {
  if (held.take !== hotkey.take) return state;
  if (held.shot) return received(state, held.shot, hotkey.seq);
  return held.error === null
    ? replaced(state, hotkey.seq)
    : takeFailed(state, hotkey.seq, held.error);
}

/** The fetched shot `id` is drawn; any other state or id is unchanged. */
export function drawn(state: ShotState, id: number): ShotState {
  return state.kind === 'fetched' && state.info.id === id
    ? { kind: 'shown', info: state.info }
    : state;
}

/** The take `seq` failed with `message`; an older take's failure changes nothing. */
export function takeFailed(state: ShotState, seq: number, message: string): ShotState {
  return state.kind === 'taking' && state.seq === seq ? { kind: 'failed', message } : state;
}

/** The fetched shot `id` could not be drawn; any other state or id is unchanged. */
export function drawFailed(state: ShotState, id: number, message: string): ShotState {
  return state.kind === 'fetched' && state.info.id === id ? { kind: 'failed', message } : state;
}

/** A question was sent: a shown shot went with it. */
export function afterSend(state: ShotState): ShotState {
  return state.kind === 'shown' ? { kind: 'sent', info: state.info } : state;
}

/** The id a send carries: only a shot the page has drawn. */
export function sendableShot(state: ShotState): number | null {
  return state.kind === 'shown' ? state.info.id : null;
}

/** A take or a draw in progress: Send and Enter wait for it. */
export function sendBlocked(state: ShotState): boolean {
  return state.kind === 'taking' || state.kind === 'fetched';
}
