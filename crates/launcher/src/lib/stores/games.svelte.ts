import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Game {
  id: string;
  name: string;
  source: "steam" | "epic" | "gog" | "manual";
  source_id: string | null;
  exe_name: string;
  exe_path: string | null;
  install_dir: string | null;
  cover_art_path: string | null;
  last_played: string | null;
  play_time_minutes: number;
}

export type FilterSource = "all" | "steam" | "epic" | "gog";

let games = $state<Game[]>([]);
let selectedGameId = $state<string | null>(null);
let filterSource = $state<FilterSource>("all");
let searchQuery = $state<string>("");
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
    const result = await invoke<Game[]>("get_games");
    games = result;
    if (result.length > 0 && selectedGameId === null) {
      selectedGameId = result[0].id;
    }
  } catch (err) {
    console.error("Failed to load games:", err);
    error = String(err);
  } finally {
    isLoading = false;
  }
}

export async function scanGames(): Promise<void> {
  isLoading = true;
  error = null;
  try {
    const result = await invoke<Game[]>("scan_games");
    games = result;
    if (result.length > 0 && selectedGameId === null) {
      selectedGameId = result[0].id;
    }
  } catch (err) {
    console.error("Failed to scan games:", err);
    error = String(err);
  } finally {
    isLoading = false;
  }
}

export function getFilteredGames(): Game[] {
  return games.filter((g) => {
    const matchesSource =
      filterSource === "all" || g.source === filterSource;
    const matchesSearch =
      !searchQuery ||
      g.name.toLowerCase().includes(searchQuery.toLowerCase());
    return matchesSource && matchesSearch;
  });
}

export function getSelectedGame(): Game | undefined {
  return games.find((g) => g.id === selectedGameId);
}

let gameStatuses = $state<Record<string, string>>({});

// Listen for injector process exit and reset status
listen<string>("injector-finished", (event) => {
  const gameId = event.payload;
  gameStatuses = { ...gameStatuses, [gameId]: "idle" };
});

export function getGameStatus(id: string): string {
  return gameStatuses[id] ?? "idle";
}

export async function launchGame(gameId: string): Promise<void> {
  // Duplicate-launch protection
  if (gameStatuses[gameId] === "launching" || gameStatuses[gameId] === "injecting") {
    return;
  }
  gameStatuses = { ...gameStatuses, [gameId]: "launching" };
  try {
    const result = await invoke<string>("launch_game", { gameId });
    gameStatuses = { ...gameStatuses, [gameId]: result };
  } catch (err) {
    console.error("Failed to launch game:", err);
    gameStatuses = { ...gameStatuses, [gameId]: "error" };
    error = String(err);
  }
}
