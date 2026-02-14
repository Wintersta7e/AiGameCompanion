use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use hudhook::inject::Process;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

#[derive(Parser)]
#[command(name = "injector", about = "Inject Claude Game Companion into a running game")]
struct Cli {
    /// Target process name (e.g. "Game.exe")
    #[arg(short, long)]
    process: Option<String>,

    /// Path to the overlay DLL (defaults to overlay.dll next to this exe)
    #[arg(short, long)]
    dll: Option<PathBuf>,

    /// Seconds to wait for the process to appear (0 = no retry)
    #[arg(short, long, default_value = "0")]
    timeout: u64,

    /// List running processes and exit
    #[arg(long)]
    list: bool,
}

fn list_processes() -> Result<Vec<String>> {
    let mut names = Vec::new();
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
                names.push(name);

                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list {
        let processes = list_processes()?;
        println!("Running processes:");
        for name in &processes {
            println!("  {name}");
        }
        return Ok(());
    }

    let dll_path = match cli.dll {
        Some(path) => path,
        None => {
            let mut path = std::env::current_exe().context("Failed to get exe path")?;
            path.pop();
            path.push("overlay.dll");
            path
        }
    };

    let dll_path = dll_path
        .canonicalize()
        .with_context(|| format!("DLL not found: {}", dll_path.display()))?;

    let process_name = cli
        .process
        .context("Missing required argument: --process (-p). Use --list to see running processes.")?;

    println!("Looking for process '{process_name}'...");

    let timeout = Duration::from_secs(cli.timeout);
    let start = Instant::now();

    let process = loop {
        match Process::by_name(&process_name) {
            Ok(p) => break p,
            Err(_) => {
                if cli.timeout == 0 || start.elapsed() >= timeout {
                    eprintln!("Process '{}' not found.", process_name);
                    eprintln!();
                    eprintln!("Hint: use --list to see running processes, or --timeout N to wait.");

                    // Show similar process names as suggestions
                    if let Ok(processes) = list_processes() {
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
