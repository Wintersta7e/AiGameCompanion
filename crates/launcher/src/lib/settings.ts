/** The launcher settings as `get_settings` returns them; mirrors Rust's `LauncherSettings`. */
export interface LauncherSettings {
  scan_on_startup: boolean;
  minimize_to_tray: boolean;
  launch_on_startup: boolean;
  active_provider: string;
  gemini_model: string;
  // Shown and switched only in the overlay; Save keeps the stored value.
  hints_first?: boolean;
}
