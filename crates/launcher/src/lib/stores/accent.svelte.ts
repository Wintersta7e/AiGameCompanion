/**
 * Live accent store. Holds the current accent colour (driven by the selected
 * game's cover art) and writes it to the document root as `--accent`, which
 * every component reads. Results are cached per game id; the hashed-hue
 * fallback is applied instantly so there's never an un-themed flash.
 */
import type { Game } from './games.svelte';
import { dominantAccent, hashHue } from '../utils/accent';

let accent = $state<string>('#e0a23c');
// Plain non-reactive memo (never read in a template/$derived), so SvelteMap isn't needed.
// eslint-disable-next-line svelte/prefer-svelte-reactivity
const cache = new Map<string, string>();
// Latest game we were asked to theme for; guards async cover-art extraction
// against a stale result landing after the user has switched games.
let currentGameId: string | null = null;

export function getAccent(): string {
  return accent;
}

function apply(value: string): void {
  accent = value;
  document.documentElement.style.setProperty('--accent', value);
}

export function setAccentFromGame(game: Game | undefined): void {
  if (!game) return;
  currentGameId = game.id;

  const cached = cache.get(game.id);
  if (cached) {
    apply(cached);
    return;
  }

  // Instant fallback so the UI is never un-themed…
  apply(hashHue(game.id));

  // …then refine from the cover art if we can read it.
  if (game.cover_art_path) {
    dominantAccent(game.cover_art_path)
      .then((c) => {
        cache.set(game.id, c);
        // Only apply if this game is still the current selection.
        if (currentGameId === game.id) apply(c);
      })
      .catch(() => {
        cache.set(game.id, hashHue(game.id));
      });
  } else {
    cache.set(game.id, hashHue(game.id));
  }
}
