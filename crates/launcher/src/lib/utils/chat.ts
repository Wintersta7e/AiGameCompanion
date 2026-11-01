// The saved chat the overlay shows, as Rust's `load_chat` returns it.

import type { Provider } from '../stores/companion.svelte';
import type { SearchReport } from './search';

/** Whether the chat on screen is being saved, and if not, why. */
type SaveState = { state: 'on' } | { state: 'read_only'; reason: 'library' | 'chat_file' };

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
