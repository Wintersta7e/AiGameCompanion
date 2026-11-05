// The saved chat the overlay shows, as Rust's `load_chat` returns it.

import type { Provider } from '../stores/companion.svelte';
import type { SearchReport } from './search';

/** Whether the chat on screen is being saved, and if not, why. */
export type SaveState =
  | { state: 'on' | 'off' | 'failed' | 'skipped' }
  | { state: 'read_only'; reason: 'library' | 'chat_file' };

export interface SavedExchange {
  savedAt: string;
  question: string;
  answer: string;
  provider: Provider;
  model: string;
  screenshot: boolean;
  search?: SearchReport;
}

export interface ChatView {
  displayGeneration: number;
  exchanges: SavedExchange[];
  save: SaveState;
}

/** What the page does with a view: clear the chat, replace it, or only update its status. */
export type ChatViewAction = 'empty' | 'replace' | 'status';

/**
 * Rust numbers each chat it puts on screen; the page shows `current`. No view
 * empties the chat, another number replaces it, and the same number keeps the
 * messages on screen (and where the list is scrolled) as they are.
 */
export function chatViewAction(current: number, view: ChatView | null): ChatViewAction {
  if (!view) return 'empty';
  return view.displayGeneration === current ? 'status' : 'replace';
}

/**
 * How long after the newest answer the chat offers a recap.
 * @internal
 */
export const RECAP_OFFER_AFTER_MS = 4 * 60 * 60 * 1000;

export interface RecapOfferInput {
  /** Answers in the chat that completed. */
  exchanges: number;
  /** When the newest of them completed, in ms since the epoch. */
  newestAnswerAt: number | null;
  now: number;
  canSend: boolean;
  asking: boolean;
}

/**
 * The resume line shows once the newest answer is `RECAP_OFFER_AFTER_MS` old;
 * its recap chip only while a question could be sent.
 */
export function recapOffer(input: RecapOfferInput): { line: boolean; chip: boolean } {
  const line =
    input.exchanges >= 1 &&
    input.newestAnswerAt !== null &&
    input.now - input.newestAnswerAt >= RECAP_OFFER_AFTER_MS;
  return { line, chip: line && input.canSend && !input.asking };
}

/** The save status line's tooltip: what is saved, where, for how long, and how to remove it. */
export const SAVE_STATUS_TOOLTIP =
  'Chat text for this game is saved on this PC for up to about two years — never screenshots. ' +
  'Up to the last 20 exchanges are sent with each question. ' +
  "New chat deletes this game's saved chat; Settings can turn saving off or delete all saved chats.";

/** The chat footer's line saying whether answers are saved; `null` before the chat arrives. */
export function saveStatusText(save: SaveState | null, linked: boolean): string {
  if (!linked) return 'No linked game — nothing saved';
  if (!save) return '';
  switch (save.state) {
    case 'on':
      return 'Saved on this PC';
    case 'off':
      return 'Not saved — kept until you quit';
    case 'failed':
      return 'Last answer not saved — retrying with the next one';
    case 'skipped':
      return 'This answer is too long to save';
    case 'read_only':
      return save.reason === 'library'
        ? "Not saving — the library can't be read; saved chats are untouched"
        : "Not saving — this game's saved chat can't be read; kept until you quit";
  }
}
