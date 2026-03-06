import { invoke } from "@tauri-apps/api/core";

export interface Game {
  id: string;
  name: string;
  source: "steam" | "epic" | "gog" | "manual";
  source_id: string | null;
  exe_name: string;
  exe_path: string | null;
  cover_art_path: string | null;
  last_played: string | null;
  play_time_minutes: number;
}

let games = $state<Game[]>([]);
let selectedGameId = $state<string | null>(null);
let filterSource = $state<string>("all");
let searchQuery = $state<string>("");

export function getGames(): Game[] {
  return games;
}

export function getSelectedGameId(): string | null {
  return selectedGameId;
}

export function getFilterSource(): string {
  return filterSource;
}

export function getSearchQuery(): string {
  return searchQuery;
}

export function setSelectedGameId(id: string | null): void {
  selectedGameId = id;
}

export function setFilterSource(source: string): void {
  filterSource = source;
}

export function setSearchQuery(query: string): void {
  searchQuery = query;
}

export async function scanGames(): Promise<void> {
  try {
    const result = await invoke<Game[]>("scan_games");
    games = result;
    if (result.length > 0 && selectedGameId === null) {
      selectedGameId = result[0].id;
    }
  } catch (err) {
    console.error("Failed to scan games:", err);
  }
}

export async function loadGames(): Promise<void> {
  try {
    const result = await invoke<Game[]>("get_games");
    games = result;
    if (result.length > 0 && selectedGameId === null) {
      selectedGameId = result[0].id;
    }
  } catch (err) {
    console.error("Failed to load games:", err);
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
