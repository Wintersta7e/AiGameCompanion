import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export interface Game {
  id: string;
  name: string;
  source: 'steam' | 'epic' | 'gog' | 'manual';
  source_id: string | null;
  exe_name: string;
  exe_path: string | null;
  install_dir: string | null;
  cover_art_path: string | null;
  last_played: string | null;
  play_time_minutes: number;
}

export type FilterSource = 'all' | 'steam' | 'epic' | 'gog';

let games = $state<Game[]>([]);
let selectedGameId = $state<string | null>(null);
let filterSource = $state<FilterSource>('all');
let searchQuery = $state<string>('');
let isLoading = $state<boolean>(true);
let error = $state<string | null>(null);

export function getGames(): Game[] {
  return games;
}

export function getSelectedGameId(): string | null {
  return selectedGameId;
}

export function getFilterSource(): FilterSource {
  return filterSource;
}

export function getSearchQuery(): string {
  return searchQuery;
}

export function getIsLoading(): boolean {
  return isLoading;
}

export function getError(): string | null {
  return error;
}

export function setSelectedGameId(id: string | null): void {
  selectedGameId = id;
}

export function setFilterSource(source: FilterSource): void {
  filterSource = source;
}

export function setSearchQuery(query: string): void {
  searchQuery = query;
}

export async function loadGames(): Promise<void> {
  isLoading = true;
  error = null;
  try {
    const result = await invoke<Game[]>('get_games');
    games = result;
    if (result.length > 0 && selectedGameId === null) {
      selectedGameId = result[0].id;
    }
  } catch (err) {
    console.error('Failed to load games:', err);
    error = String(err);
  } finally {
    isLoading = false;
  }
}

export async function scanGames(): Promise<void> {
  isLoading = true;
  error = null;
  try {
    const result = await invoke<Game[]>('scan_games');
    games = result;
    if (result.length > 0 && selectedGameId === null) {
      selectedGameId = result[0].id;
    }
  } catch (err) {
    console.error('Failed to scan games:', err);
    error = String(err);
  } finally {
    isLoading = false;
  }
}

export function getFilteredGames(): Game[] {
  return games.filter((g) => {
    const matchesSource = filterSource === 'all' || g.source === filterSource;
    const matchesSearch = !searchQuery || g.name.toLowerCase().includes(searchQuery.toLowerCase());
    return matchesSource && matchesSearch;
  });
}

export function getSelectedGame(): Game | undefined {
  return games.find((g) => g.id === selectedGameId);
}

let gameStatuses = $state<Record<string, string>>({});

listen<string>('game-linked', (event) => {
  const gameId = event.payload;
  gameStatuses = { ...gameStatuses, [gameId]: 'linked' };
});

// Reset status when the watched game process exits (or was never found).
listen<string>('game-finished', (event) => {
  const gameId = event.payload;
  gameStatuses = { ...gameStatuses, [gameId]: 'idle' };
});

export function getGameStatus(id: string): string {
  return gameStatuses[id] ?? 'idle';
}

export async function launchGame(gameId: string): Promise<void> {
  // Duplicate-launch protection: already starting or running.
  if (gameStatuses[gameId] === 'launching' || gameStatuses[gameId] === 'linked') {
    return;
  }
  // Optimistic 'launching'; the game-linked / game-finished events drive the
  // rest. Do not overwrite with the invoke result -- an event may already have
  // updated the status while we awaited.
  gameStatuses = { ...gameStatuses, [gameId]: 'launching' };
  try {
    await invoke<string>('launch_game', { gameId });
  } catch (err) {
    console.error('Failed to launch game:', err);
    gameStatuses = { ...gameStatuses, [gameId]: 'error' };
    error = String(err);
  }
}
