use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use hudhook::inject::Process;
use serde::Deserialize;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

#[derive(Parser)]
#[command(name = "injector", about = "AI Game Companion -- DLL injector")]
struct Cli {
    /// Target process name (e.g. "Game.exe") -- one-shot inject
    #[arg(short, long)]
    process: Option<String>,

    /// Path to the overlay DLL (defaults to overlay.dll next to this exe)
    #[arg(short, long)]
    dll: Option<PathBuf>,

    /// Seconds to wait for the process to appear (0 = no retry, manual mode only)
    #[arg(short, long, default_value = "0")]
    timeout: u64,

    /// Path to config.toml (defaults to config.toml next to this exe)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// List running processes and exit
    #[arg(long)]
    list: bool,
}

// --- Config structs ---

#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    games: Vec<GameEntry>,
}

#[derive(Deserialize, Clone)]
struct GameEntry {
    name: Option<String>,
    process: String,
}

impl GameEntry {
    fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.process)
    }
}

fn load_config(cli_path: Option<&PathBuf>) -> Config {
    let config_path = match cli_path {
        Some(p) => p.clone(),
        None => {
            let Ok(mut exe) = std::env::current_exe() else {
                return Config::default();
            };
            exe.pop();
            exe.push("config.toml");
            exe
        }
    };

    let contents = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return Config::default(),
    };

    match toml::from_str(&contents) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: failed to parse config.toml: {e}");
            Config::default()
        }
    }
}

// --- Process enumeration ---

struct ProcessInfo {
    name: String,
    pid: u32,
}

fn enumerate_processes() -> Result<Vec<ProcessInfo>> {
    let mut procs = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .context("Failed to create process snapshot")?;

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let zero_idx = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..zero_idx]);
                procs.push(ProcessInfo {
                    name,
                    pid: entry.th32ProcessID,
                });

                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
    }
    Ok(procs)
}

fn list_process_names() -> Result<Vec<String>> {
    let mut names: Vec<String> = enumerate_processes()?
        .into_iter()
        .map(|p| p.name)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

fn timestamp() -> String {
    chrono::Local::now().format("[%H:%M:%S]").to_string()
}

// --- DLL path resolution ---

fn resolve_dll_path(cli_dll: Option<PathBuf>) -> Result<PathBuf> {
    let dll_path = match cli_dll {
        Some(path) => path,
        None => {
            let mut path = std::env::current_exe().context("Failed to get exe path")?;
            path.pop();
            path.push("overlay.dll");
            path
        }
    };
    dll_path
        .canonicalize()
        .with_context(|| format!("DLL not found: {}", dll_path.display()))
}

// --- One-shot inject (manual mode) ---

fn inject_one_shot(process_name: &str, dll_path: PathBuf, timeout_secs: u64) -> Result<()> {
    println!("Looking for process '{process_name}'...");

    let timeout = Duration::from_secs(timeout_secs);
    let start = Instant::now();

    let process = loop {
        match Process::by_name(process_name) {
            Ok(p) => break p,
            Err(_) => {
                if timeout_secs == 0 || start.elapsed() >= timeout {
                    eprintln!("Process '{}' not found.", process_name);
                    eprintln!();
                    eprintln!("Hint: use --list to see running processes, or --timeout N to wait.");

                    if let Ok(processes) = list_process_names() {
                        let query = process_name.to_lowercase();
                        let similar: Vec<_> = processes
                            .iter()
                            .filter(|p| p.to_lowercase().contains(&query.replace(".exe", "")))
                            .collect();
                        if !similar.is_empty() {
                            eprintln!();
                            eprintln!("Similar processes:");
                            for name in similar {
                                eprintln!("  {name}");
                            }
                        }
                    }

                    bail!("Process not found");
                }
                thread::sleep(Duration::from_secs(1));
                print!(".");
            }
        }
    };

    println!();
    println!("Injecting {} into {}...", dll_path.display(), process_name);

    process
        .inject(dll_path)
        .context("Failed to inject DLL")?;

    println!("Injection successful!");
    Ok(())
}

// --- Watch mode ---

fn watch_mode(games: Vec<GameEntry>, dll_path: PathBuf) -> Result<()> {
    println!("AI Game Companion -- Injector");
    println!("Watching for:");
    for game in &games {
        println!("  - {} ({})", game.display_name(), game.process);
    }
    println!("Press Ctrl+C to stop.");
    println!();

    // Map process name (lowercase) -> GameEntry for fast lookup
    let game_map: HashMap<String, &GameEntry> = games
        .iter()
        .map(|g| (g.process.to_lowercase(), g))
        .collect();

    // Track which PIDs we've already injected into
    let mut injected_pids: HashSet<u32> = HashSet::new();
    // Track which game process names have an active injected PID
    let mut active_injections: HashMap<String, u32> = HashMap::new();

    loop {
        let procs = match enumerate_processes() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{} Failed to enumerate processes: {e}", timestamp());
                thread::sleep(Duration::from_secs(3));
                continue;
            }
        };

        let current_pids: HashSet<u32> = procs.iter().map(|p| p.pid).collect();

        // Check for exited games
        let exited: Vec<String> = active_injections
            .iter()
            .filter(|(_, pid)| !current_pids.contains(pid))
            .map(|(name, _)| name.clone())
            .collect();

        for proc_lower in exited {
            let pid = active_injections.remove(&proc_lower).unwrap();
            injected_pids.remove(&pid);
            if let Some(game) = game_map.get(&proc_lower) {
                println!("{} {} exited -- will re-inject on next launch", timestamp(), game.display_name());
            }
        }

        // Check for new games to inject
        for proc_info in &procs {
            let proc_lower = proc_info.name.to_lowercase();

            if !game_map.contains_key(&proc_lower) {
                continue;
            }
            if injected_pids.contains(&proc_info.pid) {
                continue;
            }

            let game = game_map[&proc_lower];
            println!("{} Found {} (PID {}) -- injecting...", timestamp(), game.display_name(), proc_info.pid);

            match Process::by_name(&game.process) {
                Ok(process) => match process.inject(dll_path.clone()) {
                    Ok(()) => {
                        println!("{} Injected into {} (PID {})", timestamp(), game.display_name(), proc_info.pid);
                        injected_pids.insert(proc_info.pid);
                        active_injections.insert(proc_lower, proc_info.pid);
                    }
                    Err(e) => {
                        eprintln!("{} Failed to inject into {}: {e}", timestamp(), game.display_name());
                    }
                },
                Err(e) => {
                    eprintln!("{} Failed to open {}: {e}", timestamp(), game.display_name());
                }
            }
        }

        thread::sleep(Duration::from_secs(3));
    }
}

// --- Main ---

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list {
        let processes = list_process_names()?;
        println!("Running processes:");
        for name in &processes {
            println!("  {name}");
        }
        return Ok(());
    }

    let dll_path = resolve_dll_path(cli.dll)?;

    // Manual mode: --process flag given
    if let Some(process_name) = cli.process {
        return inject_one_shot(&process_name, dll_path, cli.timeout);
    }

    // Watch mode: check config for [[games]]
    let config = load_config(cli.config.as_ref());

    if config.games.is_empty() {
        eprintln!("No --process flag and no [[games]] entries in config.toml.");
        eprintln!();
        eprintln!("Usage:");
        eprintln!("  injector.exe --process \"Game.exe\"    One-shot inject");
        eprintln!("  Add [[games]] to config.toml          Watch mode (auto-inject)");
        eprintln!("  injector.exe --list                   List running processes");
        bail!("Nothing to do");
    }

    watch_mode(config.games, dll_path)
}
