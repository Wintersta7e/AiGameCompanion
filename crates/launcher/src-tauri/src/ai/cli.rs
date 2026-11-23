//! In-process Claude / Codex CLI streaming. Spawns the provider CLI directly
//! (no localhost HTTP proxy), feeds it the chat history on stdin (in WSL,
//! through a temp file the script redirects), and forwards each decoded text
//! chunk to a caller-supplied callback. The child is spawned
//! with `kill_on_drop` so aborting the owning task terminates the CLI process.

use std::fmt::Write as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::ChatMessage;

/// Default Claude model when the user has not configured one. Codex ignores the
/// model (that CLI rejects an explicit `-m`), so no default is needed there.
pub(crate) const DEFAULT_CLAUDE_MODEL: &str = "claude-haiku-4-5";

/// Name of the Codex working directory: `~/.cache/<name>` in WSL, and a
/// folder of that name in the app's local data folder on Windows.
const CODEX_WORKDIR: &str = "aigc-codex-workdir";
/// Cap on total stdout bytes from a CLI child, matching the Gemini stream cap.
const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024;
/// How many stderr lines to keep for the failure message.
const STDERR_TAIL_LINES: usize = 5;
/// How many bytes of each stderr line are kept; the rest is read and dropped.
const STDERR_LINE_BYTES: usize = 4096;
/// Folder, under the app's local data directory, that holds a Codex
/// screenshot while its request runs.
pub(crate) const SHOTS_DIR: &str = "shots";
/// Marker printed by the WSL shell immediately before the CLI runs, on stdout
/// and on stderr.
///
/// `wsl_bash` sources the user's profile and interactive `.bashrc`, which is
/// where many installs put the CLI on PATH -- but it is also where
/// nvm/fastfetch/"you have mail" banners print, and Codex output is plain
/// text, so those lines were parsed as the start of the model's answer.
/// Everything up to and including this marker is discarded, which keeps the
/// PATH without the noise. A run that never prints it never reached the CLI.
const WSL_SENTINEL: &str = "__AIGC_STREAM_BEGIN__";
/// Printed on stderr instead of `WSL_SENTINEL` when `wslpath` cannot map the
/// Windows path of a request's file: the drive is not reachable from WSL.
const WSL_PATH_UNMAPPED: &str = "__AIGC_PATH_UNMAPPED__";

/// Windows `CREATE_NO_WINDOW` flag -- prevents console popups from `wsl.exe` and
/// other console-subsystem processes.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How to invoke a CLI tool.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliMode {
    /// Not available on this system.
    #[default]
    Unavailable,
    /// Available directly on the Windows PATH.
    Native,
    /// Available inside WSL (invoke via `wsl.exe`).
    Wsl,
}

impl CliMode {
    pub(crate) const fn is_available(self) -> bool {
        !matches!(self, Self::Unavailable)
    }

    /// Human label for where the CLI was detected.
    pub(crate) const fn location(self) -> &'static str {
        match self {
            Self::Native => "PATH",
            Self::Wsl => "WSL",
            Self::Unavailable => "",
        }
    }
}

/// Cached CLI availability, detected once at startup on a background thread.
#[derive(Debug, Clone, Default)]
pub(crate) struct CliConfig {
    pub claude: CliMode,
    pub codex: CliMode,
    pub codex_workdir: String,
}

/// Which content the parser decoded from one CLI stdout line.
#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Text(String),
    Error(String),
}

/// The WSL user's home directory, resolved once per run.
///
/// Scripts interpolate this path instead of writing `$HOME`. Under the old
/// `wsl.exe --` form the command line went through the user's default shell
/// first, which expanded every `$VAR`, `$(...)` and backtick before `bash -lic`
/// ran -- a `$HOME` in the script was that shell's value, not bash's, and the
/// Codex workdir resolved to `""`. `wsl_exec` removes that shell; the passwd
/// database still needs no environment to answer.
///
/// Only a resolved home is kept. A failed lookup -- WSL still starting at
/// logon, or installed after the launcher started -- is asked again by the
/// next detection, so Re-check can find Codex without a restart.
fn wsl_home(limit: std::time::Duration) -> Option<&'static str> {
    let cache = home_cache();
    if let Some(home) = cache.get() {
        return Some(home);
    }
    let out = output_within(
        windowless(&mut wsl_exec(&[
            "sh",
            "-c",
            r#"getent passwd "$(id -u)" | cut -d: -f6"#,
        ])),
        "",
        limit,
    )?;
    let home = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || !home.starts_with('/') {
        tracing::warn!(
            "Could not resolve the WSL home (exit {}): stdout [{home}], stderr [{}]",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return None;
    }
    tracing::info!("WSL home resolved: {home}");
    Some(cache.get_or_init(|| home))
}

/// Where `wsl_home` keeps what it resolved: one per process, or one per fake
/// `wsl.exe` in a test.
fn home_cache() -> &'static std::sync::OnceLock<String> {
    static HOME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    #[cfg(test)]
    if let Some(fake) = tests::fake_wsl() {
        return fake.home;
    }
    &HOME
}

/// Run a detection child to its end with stdout and stderr captured, or kill
/// it once `limit` has passed (`None`, logged). A `wsl.exe` that never answers
/// -- a distro stuck starting, a profile waiting for input -- otherwise held
/// detection, and with it every Re-check, forever. The pipes are read on
/// their own threads, which are not waited for once the child is gone: a
/// process the child started may still hold them open. `input` is written to
/// the child's stdin, also on a thread of its own; empty means no stdin.
fn output_within(
    cmd: &mut std::process::Command,
    input: &str,
    limit: std::time::Duration,
) -> Option<std::process::Output> {
    let started = std::time::Instant::now();
    let stdin = if input.is_empty() {
        std::process::Stdio::null()
    } else {
        std::process::Stdio::piped()
    };
    let mut child = cmd
        .stdin(stdin)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| tracing::debug!("{:?} did not start: {e}", cmd.get_program()))
        .ok()?;
    if let Some(mut pipe) = child.stdin.take() {
        let input = input.to_owned();
        // A child that exits without reading closes the pipe: not an error.
        std::thread::spawn(move || drop(std::io::Write::write_all(&mut pipe, input.as_bytes())));
    }
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < limit => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            outcome => {
                tracing::warn!(
                    "{:?} gave no answer within {}s ({outcome:?}); ending it",
                    cmd.get_program(),
                    limit.as_secs()
                );
                crate::util::log_if_err("end a detection child", child.kill());
                crate::util::log_if_err("reap a detection child", child.wait());
                return None;
            }
        }
    };
    let collect = |pipe: Option<std::sync::mpsc::Receiver<Vec<u8>>>| {
        let left = limit.saturating_sub(started.elapsed());
        pipe.and_then(|rx| rx.recv_timeout(left).ok())
            .unwrap_or_default()
    };
    Some(std::process::Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// Read `pipe` to its end on a thread of its own, at most 64 KiB of it.
fn drain(pipe: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        crate::util::log_if_err(
            "read a detection pipe",
            std::io::Read::read_to_end(&mut std::io::Read::take(pipe, 64 * 1024), &mut out),
        );
        // The receiver is gone when the child outlived its deadline.
        drop(tx.send(out));
    });
    rx
}

/// Hide the console window without touching the pipes, for probes whose output
/// we actually read. Discarding stdout here once made the Codex workdir probe
/// return an empty path on every run.
#[cfg_attr(
    not(windows),
    expect(
        clippy::missing_const_for_fn,
        reason = "the Windows build calls creation_flags, which is not const"
    )
)]
fn windowless(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Windows `CREATE_SUSPENDED`: a CLI child starts paused, so it is in its job
/// before it can start a process of its own.
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// The Job Object a CLI child runs in on Windows. Every process the child
/// starts joins it, and closing it ends them all: dropping it when a request
/// ends (answer, error, Stop, a newer request, a timeout), or the launcher
/// exiting by any path. An npm-installed CLI is `node` plus the CLI's own
/// binary, which `node` starts, and `kill_on_drop` ends only the first.
#[cfg(windows)]
mod tree {
    #![expect(
        unsafe_code,
        reason = "this module wraps the Win32 job object and thread calls"
    )]

    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    /// A kill-on-close job. Dropping it closes the handle, which ends every
    /// process in it.
    pub(super) struct ChildTree(OwnedHandle);

    /// Put the suspended `child` in a new kill-on-close job, then let it run.
    /// A child that cannot be put in a job still runs, as before the job
    /// existed; one that cannot be resumed is an error, and dropping it kills it.
    pub(super) fn start(
        child: &tokio::process::Child,
        label: &str,
    ) -> Result<Option<ChildTree>, String> {
        let job = child.raw_handle().map(|raw| adopt(HANDLE(raw)));
        let tree = match job {
            Some(Ok(tree)) => Some(tree),
            Some(Err(e)) => {
                tracing::warn!(
                    "{label} CLI runs outside a job; Stop ends only its first process: {e}"
                );
                None
            }
            None => None,
        };
        let pid = child
            .id()
            .ok_or_else(|| format!("{label} CLI exited before it started."))?;
        resume(pid).map_err(|e| format!("Failed to start {label} CLI: {e}"))?;
        Ok(tree)
    }

    fn adopt(process: HANDLE) -> windows::core::Result<ChildTree> {
        // SAFETY: no security attributes and no name: plain values only.
        let raw = unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null()) }?;
        // SAFETY: `raw` is a new handle nothing else owns or closes.
        let tree = ChildTree(unsafe { OwnedHandle::from_raw_handle(raw.0) });
        let job = HANDLE(tree.0.as_raw_handle());
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `job` is open, and `limits` is a live local of the size passed.
        unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(size_of_val(&limits)).unwrap_or(0),
            )
        }?;
        // SAFETY: both handles are open: `job` above, `process` owned by the child.
        unsafe { AssignProcessToJobObject(job, process) }?;
        Ok(tree)
    }

    /// Resume the threads of the suspended process `pid`: its one main thread.
    fn resume(pid: u32) -> Result<(), String> {
        // SAFETY: plain values; the snapshot handle is closed below.
        let snapshot =
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(|e| e.to_string())?;
        let mut entry = THREADENTRY32 {
            dwSize: u32::try_from(size_of::<THREADENTRY32>()).unwrap_or(0),
            ..Default::default()
        };
        let mut resumed = 0usize;
        // SAFETY: `snapshot` is open, and `entry` is a live, `dwSize`-initialised local.
        let mut more = unsafe { Thread32First(snapshot, &raw mut entry) }.is_ok();
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: plain values; the thread handle is closed below.
                let opened =
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) };
                if let Ok(thread) = opened {
                    // SAFETY: `thread` is the open handle from above.
                    if unsafe { ResumeThread(thread) } != u32::MAX {
                        resumed += 1;
                    }
                    // SAFETY: `thread` is not used again.
                    crate::util::log_if_err("CloseHandle(thread)", unsafe { CloseHandle(thread) });
                }
            }
            // SAFETY: as for Thread32First above.
            more = unsafe { Thread32Next(snapshot, &raw mut entry) }.is_ok();
        }
        // SAFETY: `snapshot` is not used again.
        crate::util::log_if_err("CloseHandle(snapshot)", unsafe { CloseHandle(snapshot) });
        if resumed == 0 {
            return Err(format!("no thread of process {pid} could be resumed"));
        }
        Ok(())
    }
}

/// Escape a string for use inside a `bash -c` / `bash -lic` command.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build every `wsl.exe` child here, as `wsl.exe --exec <program> <args...>`.
///
/// `--exec` hands the arguments to the Linux program as argv. The `--` form
/// passes the command line to the user's default shell instead, which re-parsed
/// it before `bash` ran: `$(...)`, backticks and `$VAR` expanded, `\\` collapsed
/// and a line after a newline ran, so `shell_escape` was not the only quoting
/// layer and text such as a window title could run commands. Measured with ten
/// hostile inputs: `--` altered six, `--exec` none.
fn wsl_exec(args: &[&str]) -> std::process::Command {
    let cmd = std::process::Command::new("wsl.exe");
    #[cfg(test)]
    let cmd = tests::fake_wsl().map_or(cmd, |fake| {
        let mut cmd = std::process::Command::new(fake.program);
        cmd.args(fake.script);
        cmd
    });
    let mut cmd = cmd;
    cmd.arg("--exec").args(args);
    cmd
}

/// `wsl.exe --exec bash --noediting -lis`, which reads its script from stdin
/// (`wsl_stdin`) once the profile and `.bashrc` have run.
///
/// The script travels on stdin, not as `-c`: a profile that `exec`s another
/// shell (`exec zsh`, `exec sh`) drops a `-c` script, and the new shell then
/// read stdin -- the chat -- as commands. Now that shell reads the script, so
/// it still runs, and the chat stays in a file (`wsl_run_script`).
/// `--noediting` keeps readline from echoing the script to stderr.
fn wsl_bash() -> std::process::Command {
    wsl_exec(&["bash", "--noediting", "-lis"])
}

/// What `wsl_bash` reads on stdin to run `body`, a one-line POSIX `sh`
/// script: the shell that reads it may be one the profile started. The
/// leading blank line is what a profile that reads one line takes; without
/// `HISTFILE` the interactive shell keeps the script out of its history.
fn wsl_stdin(body: &str) -> String {
    format!("\nunset HISTFILE; {body}\n")
}

/// The start of every script that runs a CLI: `WSL_SENTINEL`, on stdout and
/// on stderr. On stderr it ends the interactive shell's own lines (job
/// control warnings, prompts), so only the CLI's are kept. The leading
/// newline ends whatever the profile printed without one (a terminal title
/// escape, an `echo -n` banner), which otherwise shared the marker's line.
fn start_marker() -> String {
    format!("printf '\\n%s\\n' {WSL_SENTINEL}; printf '\\n%s\\n' {WSL_SENTINEL} >&2; ")
}

/// Whether `line` is the marker the script prints before the CLI. It is
/// matched at the line's end, in case output without a newline still
/// precedes it on the same line.
fn is_start_line(line: &str) -> bool {
    line.trim_end().ends_with(WSL_SENTINEL)
}

/// The version probe run inside WSL, with the CLI name quoted like any argument.
fn version_script(name: &str) -> String {
    format!(
        "{}{} --version </dev/null",
        start_marker(),
        shell_escape(name)
    )
}

/// Whether a version probe's stdout holds a line from the CLI after the
/// marker. Exit status 0 alone is not enough: a shell that ran nothing, or a
/// CLI that printed nothing, also exits 0.
fn has_version_line(stdout: &[u8]) -> bool {
    let stdout = String::from_utf8_lossy(stdout);
    let mut lines = stdout.lines();
    lines.any(is_start_line) && lines.any(|line| !line.trim().is_empty())
}

/// The command that runs the CLI `name` from the Windows PATH.
fn native_command(name: &str) -> std::process::Command {
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("PATH") {
        return native_command_in(name, &path);
    }
    std::process::Command::new(name)
}

/// `native_command` for the PATH value `path`.
///
/// `Command::new(name)` finds only `name.exe`, but npm installs a CLI as a
/// `name.cmd` batch file. That would have to run through `cmd.exe`, which
/// parses the command line again: `&`, `|` and `%VAR%` in an argument act, and
/// an argument cannot hold a line break -- Claude's system prompt is one
/// argument of many lines. So the batch file is read instead, and what it
/// runs is started directly with the same arguments.
#[cfg(windows)]
fn native_command_in(name: &str, path: &std::ffi::OsStr) -> std::process::Command {
    for dir in std::env::split_paths(path) {
        if dir.join(format!("{name}.exe")).is_file() {
            break;
        }
        let shim = dir.join(format!("{name}.cmd"));
        if !shim.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&shim).unwrap_or_default();
        if let Some(cmd) = npm_shim_command(&text, &dir) {
            return cmd;
        }
        tracing::warn!("{name}.cmd is on PATH, but what it runs could not be read from it");
        break;
    }
    std::process::Command::new(name)
}

/// What an npm batch file in `dir` runs, as a command to which the caller's
/// arguments are appended. Its last line is either `"%dp0%\<program>" %*`, or
/// `"%_prog%" "%dp0%\<script>" %*` where `_prog` is the interpreter beside the
/// batch file when there is one, else the one on PATH. Any other shape is
/// `None`: it is not run.
#[cfg(windows)]
fn npm_shim_command(shim: &str, dir: &std::path::Path) -> Option<std::process::Command> {
    let last = shim.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    let (before, target) = last
        .strip_suffix("%*")?
        .trim_end()
        .strip_suffix('"')?
        .rsplit_once("\"%dp0%\\")?;
    if target.contains(['"', '%']) {
        return None;
    }
    let target = dir.join(target);
    let before = before.trim();
    if before.is_empty() || before == "@" {
        return Some(std::process::Command::new(target));
    }
    if !before.ends_with("\"%_prog%\"") {
        return None;
    }
    let prog = shim.lines().find_map(|line| {
        line.trim()
            .strip_prefix("SET \"_prog=")?
            .strip_suffix('"')
            .filter(|prog| !prog.contains(['%', '\\', '/']))
    })?;
    let local = dir.join(format!("{prog}.exe"));
    let mut cmd = if local.is_file() {
        std::process::Command::new(local)
    } else {
        std::process::Command::new(prog)
    };
    cmd.arg(target);
    Some(cmd)
}

/// Check if a CLI tool is available, first natively on the Windows PATH (an
/// `.exe`, or an npm install: see `native_command_in`), then inside WSL, using
/// `bash -lis`.
///
/// Both `-l` and `-i` are required, and each was measured from the running
/// launcher: `-i` sources `.bashrc`, where nvm puts Codex (plain `-lc` made
/// Codex undetectable), and `-l` sources the profile, where `~/.local/bin` is
/// added -- without it Claude reported "Not found" on a machine that has it
/// installed. The script reaches that bash through `wsl_bash`, so no other
/// shell parses it first. See `WSL_SENTINEL` for how the interactive shell's
/// banner output is kept out of the model stream.
fn detect_cli(name: &str, limit: std::time::Duration) -> CliMode {
    let native = output_within(windowless(native_command(name).arg("--version")), "", limit)
        .is_some_and(|out| out.status.success());
    if native {
        return CliMode::Native;
    }

    let script = wsl_stdin(&version_script(name));
    let wsl = output_within(windowless(&mut wsl_bash()), &script, limit)
        .is_some_and(|out| out.status.success() && has_version_line(&out.stdout));
    if wsl {
        return CliMode::Wsl;
    }

    CliMode::Unavailable
}

/// Detect both CLIs and resolve the Codex working directory in one pass.
///
/// Codex without a usable workdir is reported `Unavailable`: the binary is
/// there but cannot answer, and offering it anyway surfaces as a bare "No such
/// file or directory" from the CLI long after the user picked the provider.
/// `local_data` is the app's local data folder, which holds the workdir of a
/// Codex on the Windows PATH; `None` leaves that Codex unavailable.
pub(crate) fn detect_all(local_data: Option<&std::path::Path>) -> CliConfig {
    detect_named("claude", "codex", super::DETECT_TIMEOUT, local_data)
}

/// `detect_all` for the CLIs named `claude` and `codex`, giving each child it
/// starts at most `limit`.
fn detect_named(
    claude: &str,
    codex: &str,
    limit: std::time::Duration,
    local_data: Option<&std::path::Path>,
) -> CliConfig {
    let claude = detect_cli(claude, limit);
    let detected_codex = detect_cli(codex, limit);
    let workdir = ensure_codex_workdir(detected_codex, limit, local_data);
    let (codex, codex_workdir) = workdir.map_or_else(
        || (CliMode::Unavailable, String::new()),
        |dir| (detected_codex, dir),
    );
    CliConfig {
        claude,
        codex,
        codex_workdir,
    }
}

/// Codex requires a git directory -- ensure a workdir with `git init` exists.
///
/// `None` means no usable directory: the caller must then treat Codex as
/// unavailable rather than spawn it with a path that does not exist, which the
/// CLI reports only as a bare "No such file or directory".
fn ensure_codex_workdir(
    mode: CliMode,
    limit: std::time::Duration,
    local_data: Option<&std::path::Path>,
) -> Option<String> {
    if mode == CliMode::Unavailable {
        return None;
    }
    if mode == CliMode::Wsl {
        // Under the user's home, not shared /tmp. Codex reads instruction files
        // (AGENTS.md) from its working directory, and a fixed
        // `/tmp/aigc-codex-workdir` can be pre-created by any other local user
        // -- the `[ -d dir/.git ]` guard then no-ops and every Codex answer is
        // steered by their file. `-s read-only` blocks writes, not reads.
        //
        // The path is interpolated from `wsl_home`, never written as `$HOME`.
        // Under the old `wsl.exe --` form the user's default shell expanded
        // `$HOME` before bash ran -- which is why an `export HOME=...` in the
        // same script did not help -- so this resolved to "" and Codex died on
        // a missing directory. `wsl_bash` uses `--exec`, which removes that shell.
        let home = wsl_home(limit)?;
        let dir = format!("{home}/.cache/{CODEX_WORKDIR}");
        let quoted = shell_escape(&dir);
        let script = format!(
            "mkdir -p {quoted} && chmod 700 {quoted} && \
             {{ [ -d {quoted}/.git ] || git -C {quoted} init >/dev/null; }}"
        );
        // `windowless`, not `silent`: `silent` discards stderr, and a failure
        // here must say why -- otherwise the only symptom is Codex exiting with
        // "No such file or directory" long afterwards.
        let out = output_within(windowless(&mut wsl_bash()), &wsl_stdin(&script), limit)?;
        if !out.status.success() {
            tracing::warn!(
                "Preparing the Codex workdir {dir} failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return None;
        }
        return Some(dir);
    }

    // In the user's own app folder for the same reason: `TEMP` can be a
    // folder shared by every account (`C:\Temp`), where another user could
    // pre-create the workdir and its AGENTS.md.
    let Some(local_data) = local_data else {
        tracing::warn!("Codex has no workdir: the app's local data folder is unknown");
        return None;
    };
    let dir = local_data.join(CODEX_WORKDIR);
    if !dir.exists() {
        crate::util::log_if_err("create the Codex workdir", std::fs::create_dir_all(&dir));
        let init = output_within(
            windowless(
                std::process::Command::new("git")
                    .args(["init"])
                    .current_dir(&dir),
            ),
            "",
            limit,
        );
        if !init.is_some_and(|out| out.status.success()) {
            tracing::warn!("git init of the Codex workdir failed");
        }
    }
    if dir.is_dir() {
        Some(dir.to_string_lossy().into_owned())
    } else {
        tracing::warn!("Codex workdir {} could not be created", dir.display());
        None
    }
}

/// Validate a model name: ASCII alphanumeric + hyphens, dots, underscores.
fn validate_model_name(model: &str) -> Result<(), String> {
    if model.is_empty() || model.len() > 128 {
        return Err("Invalid model name.".to_owned());
    }
    if !model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
    {
        return Err("Invalid model name.".to_owned());
    }
    Ok(())
}

fn build_claude_input(messages: &[ChatMessage], screenshot: Option<&str>) -> String {
    // Collect all messages into a single user turn. Claude stream-json expects
    // one user message; conversation history is concatenated as text context.
    let mut combined_text = String::new();
    for msg in messages {
        if !combined_text.is_empty() {
            combined_text.push('\n');
        }
        let _ = write!(combined_text, "[{}]: {}", msg.role, msg.content);
    }

    let mut content_parts = vec![serde_json::json!({
        "type": "text",
        "text": combined_text,
    })];

    if let Some(data) = screenshot {
        content_parts.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": data,
            }
        }));
    }

    let input_msg = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content_parts,
        },
        "parent_tool_use_id": null,
        "session_id": null,
    });

    let mut out = serde_json::to_string(&input_msg).unwrap_or_else(|e| {
        tracing::error!("Failed to serialize Claude input: {e}");
        String::new()
    });
    out.push('\n');
    out
}

fn build_codex_input(system_prompt: &str, messages: &[ChatMessage]) -> String {
    let mut text = String::new();
    if !system_prompt.is_empty() {
        text.push_str(system_prompt);
        text.push_str("\n\n");
    }
    for msg in messages {
        let _ = writeln!(text, "[{}]: {}", msg.role, msg.content);
    }
    text
}

/// Parse a single NDJSON line from Claude CLI stdout.
fn parse_claude_line(line: &str) -> Option<Parsed> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let msg_type = v.get("type")?.as_str()?;

    match msg_type {
        "stream_event" => {
            let delta_type = v
                .pointer("/event/delta/type")
                .and_then(serde_json::Value::as_str)?;
            if delta_type == "text_delta" {
                let text = v
                    .pointer("/event/delta/text")
                    .and_then(serde_json::Value::as_str)?;
                Some(Parsed::Text(text.to_owned()))
            } else {
                None
            }
        }
        // A successful result is the stream's end.
        "result" => claude_result_error(&v).map(Parsed::Error),
        // system, assistant, etc -- ignore.
        _ => None,
    }
}

/// The message of a Claude CLI `result` frame that reports an error, or `None`
/// when the frame reports success.
///
/// A frame is an error when `is_error` is true or its `subtype` starts with
/// `error`. The CLI writes its reason to `result` or `errors`; the HTTP status
/// or the subtype stands in only when neither carries text.
fn claude_result_error(frame: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    let subtype = frame.get("subtype").and_then(Value::as_str);
    let is_error = frame.get("is_error").and_then(Value::as_bool) == Some(true)
        || subtype.is_some_and(|subtype| subtype.starts_with("error"));
    if !is_error {
        return None;
    }
    let result = frame
        .get("result")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let errors = || {
        let list = frame.get("errors").and_then(Value::as_array)?;
        let texts: Vec<&str> = list
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect();
        (!texts.is_empty()).then(|| texts.join("; "))
    };
    let status = || {
        let status = frame.get("api_error_status").and_then(Value::as_u64)?;
        Some(format!("Claude CLI error (HTTP {status})"))
    };
    let named = || {
        let subtype = subtype.filter(|subtype| !subtype.is_empty() && *subtype != "success")?;
        Some(format!("Claude CLI error ({subtype})"))
    };
    let message = result
        .or_else(errors)
        .or_else(status)
        .or_else(named)
        .unwrap_or_else(|| "Claude CLI returned an error without a message".to_owned());
    Some(
        message
            .chars()
            .take(super::gemini::MAX_ERROR_MESSAGE_CHARS)
            .collect(),
    )
}

/// Parse a single line from Codex CLI stdout. Without `--json`, `codex exec`
/// prints only the answer there, as plain text (everything else goes to
/// stderr), so every line -- a JSON object, a blank line -- is the answer and is
/// emitted verbatim. The newline `lines()` removed is put back, so the answer
/// keeps its line breaks and blank lines (paragraphs, lists).
#[expect(
    clippy::unnecessary_wraps,
    reason = "run_cli takes one parser shape for both CLIs"
)]
fn parse_codex_line(line: &str) -> Option<Parsed> {
    Some(Parsed::Text(format!("{line}\n")))
}

/// A new screenshot file name in `dir`.
fn shot_path(dir: &std::path::Path) -> std::path::PathBuf {
    temp_path(dir, "aigc-shot", "png")
}

/// A new file name in `dir`, `<stem>-<pid>-<nanos>.<ext>`. It has to be
/// unique, not secret: the directory belongs to this user and this app.
fn temp_path(dir: &std::path::Path, stem: &str, ext: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    dir.join(format!("{stem}-{}-{nanos}.{ext}", std::process::id()))
}

/// A file written to disk for one CLI request: a screenshot for Codex, or the
/// prompt a CLI in WSL reads. Dropping it deletes the file, so it lives
/// exactly as long as the request's future.
#[derive(Debug)]
struct TempFile {
    path: std::path::PathBuf,
}

impl TempFile {
    /// Write `bytes` to a new file at exactly `path`. Fails if anything already
    /// exists there, so an existing file is never written through. The file is
    /// closed before this returns.
    fn create(path: std::path::PathBuf, bytes: &[u8]) -> Result<Self, String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("could not create the file: {e}"))?;
        let written = std::io::Write::write_all(&mut file, bytes);
        drop(file);
        // From here the guard owns the file, so a failed write removes it.
        let temp = Self { path };
        written.map_err(|e| format!("could not write the file: {e}"))?;
        Ok(temp)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            // The WSL script deletes its prompt file itself.
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                tracing::warn!("Could not delete a CLI request file: {e}");
            }
            _ => {}
        }
    }
}

const CODEX_SHOT_FAILED: &str = "The screenshot could not be handed to Codex; nothing was sent.";

/// The file Codex reads `png` from, written into `dir`. No screenshot is no
/// file; a screenshot with no folder, or one that cannot be written, is an
/// error: the request must not go out without the image the user asked for.
fn codex_shot(
    dir: Option<&std::path::Path>,
    png: Option<&[u8]>,
) -> Result<Option<TempFile>, String> {
    let Some(png) = png else {
        return Ok(None);
    };
    let dir = dir.ok_or_else(|| CODEX_SHOT_FAILED.to_owned())?;
    TempFile::create(shot_path(dir), png)
        .map(Some)
        .map_err(|e| {
            tracing::warn!("Codex screenshot could not be written: {e}");
            CODEX_SHOT_FAILED.to_owned()
        })
}

/// Delete every regular file in the shots directory: leftovers of a crash, a
/// failed delete or a killed process. Returns how many were removed; a missing
/// directory removes none.
pub(crate) fn sweep_shots(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!("Could not delete a leftover screenshot: {e}"),
        }
    }
    removed
}

/// Codex's arguments when it runs from the Windows PATH. The prompt goes on
/// stdin, never as an argument, and the `=` form keeps the image path bound to
/// its flag. `--ephemeral` keeps the prompt, the history and the screenshot out
/// of Codex's own session files. `--disable shell_tool` takes away the tool
/// that runs commands, and `--ignore-user-config` skips the user's Codex
/// config file (MCP servers, model, effort), so Codex answers with its own
/// default model.
fn codex_args(work_dir: &str, image: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-a",
        "never",
        "-s",
        "read-only",
        "--disable",
        "shell_tool",
        "-C",
        work_dir,
        "exec",
        "--skip-git-repo-check",
        "--ephemeral",
        "--ignore-user-config",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if let Some(image) = image {
        args.push(format!("--image={image}"));
    }
    args
}

/// The Codex command `wsl_run_script` runs.
fn codex_wsl_script(work_dir: &str) -> String {
    format!(
        "codex -a never -s read-only --disable shell_tool -C {} exec \
         --skip-git-repo-check --ephemeral --ignore-user-config",
        shell_escape(work_dir),
    )
}

/// The script `wsl_bash` runs for one request: `cli`, a command line with
/// each word quoted, reading its prompt from the file at the Windows path
/// `prompt` -- never from stdin, which is the script itself -- and with an
/// image, `--image=` the file at the Windows path `image`. `wslpath` turns
/// each path into a Linux one; if that fails, the CLI does not start and the
/// script prints `WSL_PATH_UNMAPPED` on stderr after `wslpath`'s own message.
/// The prompt file is deleted as soon as the CLI ends.
fn wsl_run_script(cli: &str, prompt: &str, image: Option<&str>) -> String {
    let unmapped = format!("|| {{ printf '\\n%s\\n' {WSL_PATH_UNMAPPED} >&2; exit 1; }}; ");
    let (img, flag) = image.map_or_else(
        || (String::new(), ""),
        |image| {
            (
                format!("img=$(wslpath -u {}) {unmapped}", shell_escape(image)),
                " --image=\"$img\"",
            )
        },
    );
    format!(
        "f=$(wslpath -u {}) {unmapped}{img}{}{cli}{flag} < \"$f\"; s=$?; rm -f -- \"$f\"; exit \"$s\"",
        shell_escape(prompt),
        start_marker(),
    )
}

/// Claude's arguments, the same in both modes. The prompt goes on stdin.
///
/// Sage only wants text back, so the child runs without the user's setup:
/// `--tools ''` offers no tool, `--safe-mode` skips their CLAUDE.md, plugins,
/// hooks, skills and MCP servers, `--strict-mcp-config` drops every MCP server
/// not named here (none is), `--permission-mode dontAsk` denies instead of
/// asking, and `--no-session-persistence` keeps the conversation off disk.
pub(super) fn claude_args(model: &str, system_prompt: &str) -> Vec<String> {
    [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--tools",
        "",
        "--no-session-persistence",
        "--safe-mode",
        "--strict-mcp-config",
        "--permission-mode",
        "dontAsk",
        "--model",
        model,
        "--system-prompt",
        system_prompt,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// The script `wsl_bash` runs for Claude: `claude_args`, each one quoted, so
/// the empty `--tools` value stays a literal `''`.
fn claude_wsl_script(model: &str, system_prompt: &str) -> String {
    let args: Vec<String> = claude_args(model, system_prompt)
        .iter()
        .map(|arg| shell_escape(arg))
        .collect();
    format!("claude {}", args.join(" "))
}

/// Stream a Claude response by spawning the Claude CLI in stream-json mode.
/// In WSL the prompt goes through a file in `files_dir` (see `run_in_wsl`).
pub(super) async fn stream_claude<F>(
    cfg: &CliConfig,
    model: &str,
    system_prompt: &str,
    messages: &[ChatMessage],
    screenshot: Option<&str>,
    files_dir: Option<&std::path::Path>,
    on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String> + Send,
{
    if !cfg.claude.is_available() {
        return Err("Claude CLI is not available on this system.".to_owned());
    }
    validate_model_name(model)?;

    let run = CliRun::plain("Claude", build_claude_input(messages, screenshot));
    if cfg.claude == CliMode::Wsl {
        let claude = claude_wsl_script(model, system_prompt);
        return run_in_wsl(&claude, None, files_dir, run, on_chunk, parse_claude_line).await;
    }
    let mut cmd = Command::from(native_command("claude"));
    cmd.args(claude_args(model, system_prompt));
    run_cli(&mut cmd, run, on_chunk, parse_claude_line).await
}

/// Run `cli`, a command line with each word quoted, inside WSL through
/// `wsl_run_script`. The prompt (`run.input`) is written to a file in
/// `files_dir` that the CLI reads, so the shell's stdin carries only the
/// script; the file is gone when this future ends, however it ends. Without
/// a folder, or when the file cannot be written, nothing is sent.
async fn run_in_wsl<F, P>(
    cli: &str,
    image: Option<&str>,
    files_dir: Option<&std::path::Path>,
    mut run: CliRun<'_>,
    on_chunk: F,
    parse_line: P,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String> + Send,
    P: Fn(&str) -> Option<Parsed> + Send + Sync,
{
    let label = run.label;
    let prompt = files_dir
        .ok_or_else(|| "there is no folder for it".to_owned())
        .and_then(|dir| {
            TempFile::create(temp_path(dir, "aigc-prompt", "txt"), run.input.as_bytes())
        })
        .map_err(|e| {
            tracing::warn!("{label} prompt file could not be written: {e}");
            format!("The question could not be handed to {label} in WSL; nothing was sent.")
        })?;
    let script = wsl_run_script(cli, &prompt.path.to_string_lossy(), image);
    run.script = Some(wsl_stdin(&script));
    run.skip_until = Some(WSL_SENTINEL);
    let result = run_cli(&mut Command::from(wsl_bash()), run, on_chunk, parse_line).await;
    // Only now may the file go: the CLI reads it while `run_cli` runs.
    drop(prompt);
    result
}

/// Stream a Codex response by spawning the Codex CLI in `exec` mode.
///
/// `png` is a screenshot to show Codex, written as a file into `shots_dir`
/// here and deleted when this future ends, however it ends: answer, error,
/// timeout, Stop or a newer request. When the file cannot be written (or there
/// is no folder) the request ends with an error before Codex is spawned. In
/// WSL the prompt goes through a file in `shots_dir` too (see `run_in_wsl`).
pub(super) async fn stream_codex<F>(
    cfg: &CliConfig,
    system_prompt: &str,
    messages: &[ChatMessage],
    shots_dir: Option<&std::path::Path>,
    png: Option<&[u8]>,
    on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String> + Send,
{
    if !cfg.codex.is_available() {
        return Err("Codex CLI is not available on this system.".to_owned());
    }

    let temp_shot = codex_shot(shots_dir, png)?;
    if temp_shot.is_some() {
        tracing::info!("Codex request: screenshot attached");
    }
    let image = temp_shot
        .as_ref()
        .map(|temp| temp.path.to_string_lossy().into_owned());

    let work_dir = cfg.codex_workdir.as_str();
    let run = CliRun::plain("Codex", build_codex_input(system_prompt, messages));
    let result = if cfg.codex == CliMode::Wsl {
        let codex = codex_wsl_script(work_dir);
        run_in_wsl(
            &codex,
            image.as_deref(),
            shots_dir,
            run,
            on_chunk,
            parse_codex_line,
        )
        .await
    } else {
        let mut cmd = Command::from(native_command("codex"));
        cmd.args(codex_args(work_dir, image.as_deref()));
        run_cli(&mut cmd, run, on_chunk, parse_codex_line).await
    };
    // Only now may the file go: Codex reads it while `run_cli` runs.
    drop(temp_shot);
    result
}

/// One CLI request as `run_cli` drives it.
struct CliRun<'a> {
    /// The CLI's name in messages and the log.
    label: &'a str,
    /// The prompt: written to the child's stdin, which is then closed, unless
    /// `script` is set. Lines of it the child echoes to stderr are not kept.
    input: String,
    /// Written to stdin in place of `input`: the script a WSL shell runs.
    script: Option<String>,
    /// When set, stdout up to and including this marker line is skipped.
    skip_until: Option<&'a str>,
    /// How long the child may go without writing a line before it is ended.
    idle: std::time::Duration,
}

impl<'a> CliRun<'a> {
    /// A run that writes `input` to stdin, waits for no marker and uses the
    /// production idle limit.
    const fn plain(label: &'a str, input: String) -> Self {
        Self {
            label,
            input,
            script: None,
            skip_until: None,
            idle: super::REQUEST_TIMEOUT,
        }
    }
}

/// Spawn a CLI child, write `input` to stdin, and stream parsed stdout lines to
/// `on_chunk`. stdin/stdout/stderr are driven concurrently in this one future so
/// that aborting the owning task drops the child; `kill_on_drop` then terminates
/// it, and on Windows its job ends every process it started. Note: in WSL mode
/// the direct child is `wsl.exe`, so this ends the relay but may orphan the
/// in-distro CLI process (a known limitation, same as before).
async fn run_cli<F, P>(
    cmd: &mut Command,
    run: CliRun<'_>,
    mut on_chunk: F,
    parse_line: P,
) -> Result<(), String>
where
    F: FnMut(String) -> Result<(), String> + Send,
    P: Fn(&str) -> Option<Parsed> + Send + Sync,
{
    let CliRun {
        label,
        input,
        script,
        skip_until,
        idle,
    } = run;
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn {label} CLI: {e}"))?;
    // Held until this future ends, however it ends.
    #[cfg(windows)]
    let _tree = tree::start(&child, label)?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("Failed to open {label} stdin."))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("Failed to open {label} stdout."))?;
    let stderr = child.stderr.take();

    let prompt = input.as_str();
    let stdin_text = script.as_deref().unwrap_or(prompt);
    let write_fut = async move {
        crate::util::log_if_err(
            "write the CLI prompt",
            stdin.write_all(stdin_text.as_bytes()).await,
        );
        crate::util::log_if_err("flush the CLI prompt", stdin.flush().await);
        // Dropping stdin closes the pipe so the CLI knows the input is complete.
        Ok::<(), String>(())
    };

    // Every line the child writes, on either pipe, is a sign of life: Codex
    // reports its progress on stderr and prints the answer only at the end.
    let activity = tokio::sync::Notify::new();
    let idle_watch = async {
        while tokio::time::timeout(idle, activity.notified())
            .await
            .is_ok()
        {}
    };

    let stderr_fut = read_stderr_tail(stderr, prompt, &activity);

    // Boxed: the read state would make every caller's future large.
    let read_fut = Box::pin(read_answer(
        stdout,
        label,
        skip_until,
        &activity,
        &mut on_chunk,
        parse_line,
    ));

    // Returning drops the child, which `kill_on_drop` (and its job) ends. A
    // stdout error (the size cap, an error frame) returns at once: a child
    // that outlives its closed stdout may hold stderr open for minutes.
    let joined = tokio::select! {
        joined = async {
            tokio::try_join!(
                write_fut,
                async { Ok(stderr_fut.await) },
                read_fut,
            )
        } => joined,
        () = idle_watch => {
            tracing::warn!("{label} CLI wrote nothing for {}s; ending it", idle.as_secs());
            return Err(super::TIMED_OUT.to_owned());
        }
    };
    let ((), (stderr_tail, stderr_lines), streamed) = joined?;
    tracing::debug!("{label} wrote {stderr_lines} stderr line(s)");
    let emitted = match streamed {
        Streamed::NotStarted => {
            tracing::warn!("{label} did not start in WSL: no start marker on stdout");
            return Err(not_started_message(label, &stderr_tail));
        }
        Streamed::Nothing => false,
        Streamed::Text => true,
    };

    // Stdout reaching EOF is NOT success. Without this, a CLI that fails before
    // printing anything -- not logged in, binary missing, WSL distro down --
    // reached the user as a completed, empty answer with no error at all.
    let status = child.wait().await;
    drop(child);
    let failed = |outcome: &str| cli_failed(label, outcome, &stderr_tail, stderr_lines);
    match status {
        Ok(status) if status.success() => {
            if emitted {
                Ok(())
            } else {
                failed("exited with no answer")
            }
        }
        Ok(status) => failed(&format!("exited with {status}")),
        Err(e) => Err(format!("Failed to wait for {label} CLI: {e}")),
    }
}

/// Log how the CLI ended and its error lines, never the rest of stderr (a CLI
/// may repeat the chat there), and return the error the user sees.
fn cli_failed(
    label: &str,
    outcome: &str,
    stderr_tail: &[String],
    stderr_lines: usize,
) -> Result<(), String> {
    let errors = error_lines(stderr_tail);
    let mut line = format!("{label} CLI failed: {outcome}, {stderr_lines} stderr line(s)");
    if !errors.is_empty() {
        line.push_str(": ");
        line.push_str(&errors.join(" / "));
    }
    tracing::warn!("{line}");
    Err(cli_failure_message(label, stderr_tail))
}

/// What a child's stdout held, once read to its end.
#[derive(Debug, PartialEq, Eq)]
enum Streamed {
    /// The marker `skip_until` names never came: the CLI never ran.
    NotStarted,
    /// No text arrived.
    Nothing,
    /// Text arrived.
    Text,
}

/// Read the child's stdout to its end, passing each parsed line to
/// `on_chunk`. Each line is reported to `activity`.
async fn read_answer<F, P>(
    stdout: tokio::process::ChildStdout,
    label: &str,
    skip_until: Option<&str>,
    activity: &tokio::sync::Notify,
    on_chunk: &mut F,
    parse_line: P,
) -> Result<Streamed, String>
where
    F: FnMut(String) -> Result<(), String> + Send,
    P: Fn(&str) -> Option<Parsed> + Send + Sync,
{
    let mut reader = BufReader::new(stdout);
    let mut bytes = Vec::new();
    let mut total_bytes: usize = 0;
    let mut emitted = false;
    // Set only when the command prints WSL_SENTINEL; everything the
    // interactive shell emitted before it is profile noise, not output.
    let mut waiting_for_sentinel = skip_until.is_some();
    loop {
        // Mirror the Gemini stream cap. A line is read only as far as the
        // bytes left under it, so one huge line cannot grow the launcher's
        // memory past the cap before its newline comes.
        let left = MAX_STREAM_BYTES.saturating_sub(total_bytes);
        let read = read_line_capped(&mut reader, &mut bytes, left)
            .await
            .map_err(|e| format!("Failed to read from {label} CLI: {e}"))?;
        match read {
            Line::End => break,
            Line::Cut => return Err(format!("{label} response exceeded the size limit.")),
            Line::Whole => {}
        }
        // A byte that is not UTF-8 (a banner in another locale) is shown as
        // U+FFFD: one stray byte must not fail the whole answer.
        let line = String::from_utf8_lossy(&bytes).into_owned();
        activity.notify_one();
        if waiting_for_sentinel {
            // At the line's end: see `is_start_line`.
            if skip_until.is_some_and(|marker| line.trim_end().ends_with(marker)) {
                waiting_for_sentinel = false;
            } else if !line.trim().is_empty() {
                tracing::debug!("{label}: dropping pre-start shell output: {line}");
            }
            continue;
        }
        // The `+ 1` is the newline the reader strips, so a flood of blank
        // lines counts too.
        total_bytes = total_bytes.saturating_add(line.len() + 1);
        // Blank lines reach the parser: they are paragraph breaks in a
        // plain-text answer, and Claude's JSON parser drops them anyway.
        match parse_line(&line) {
            Some(Parsed::Text(text)) => {
                emitted = true;
                on_chunk(text)?;
            }
            Some(Parsed::Error(message)) => return Err(message),
            None => {}
        }
    }
    Ok(if waiting_for_sentinel {
        Streamed::NotStarted
    } else if emitted {
        Streamed::Text
    } else {
        Streamed::Nothing
    })
}

/// Count the child's stderr lines and keep the last few: when the child fails
/// without writing any stdout, they are the only thing that can explain why.
/// Codex echoes the whole prompt, and with it the chat, to stderr, so a line of
/// the prompt is only counted, never kept. In WSL the lines up to the start
/// marker are the shell's own (job control warnings, prompts, which run into
/// the marker's line), and the login shell's `logout` follows the CLI's: all
/// of them are dropped once the marker comes.
async fn read_stderr_tail(
    stderr: Option<tokio::process::ChildStderr>,
    prompt: &str,
    activity: &tokio::sync::Notify,
) -> (Vec<String>, usize) {
    let prompt_lines: std::collections::HashSet<&str> = prompt.lines().map(str::trim).collect();
    let mut tail: Vec<String> = Vec::new();
    let mut count = 0usize;
    let mut started = false;
    if let Some(stderr) = stderr {
        let mut reader = BufReader::new(stderr);
        let mut bytes = Vec::new();
        // Only the start of a long line is kept; the rest is read and dropped.
        while let Ok(read) = read_line_capped(&mut reader, &mut bytes, STDERR_LINE_BYTES).await {
            let cut = match read {
                Line::End => break,
                Line::Whole => false,
                Line::Cut => {
                    if skip_line(&mut reader).await.is_err() {
                        break;
                    }
                    cut_to_char(&mut bytes);
                    true
                }
            };
            // Lossy: a CLI whose stderr is no longer read dies of a broken
            // pipe on its next write, so no line may end the reading.
            let line = String::from_utf8_lossy(&bytes).into_owned();
            activity.notify_one();
            if line.trim().is_empty() {
                continue;
            }
            if is_start_line(&line) {
                (started, count) = (true, 0);
                tail.clear();
                continue;
            }
            if started && line.trim() == "logout" {
                continue;
            }
            count += 1;
            // A cut line of the prompt is the start of one.
            let of_prompt = if cut {
                let head = line.trim_start();
                prompt_lines.iter().any(|p| p.starts_with(head))
            } else {
                prompt_lines.contains(line.trim())
            };
            if of_prompt {
                continue;
            }
            if tail.len() == STDERR_TAIL_LINES {
                tail.remove(0);
            }
            tail.push(line);
        }
    }
    (tail, count)
}

/// How a `read_line_capped` call ended.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// The pipe is at its end; no line was read.
    End,
    /// A whole line, or the last bytes before the end of the pipe.
    Whole,
    /// The line reached the limit before its newline; the rest is unread.
    Cut,
}

/// Read one line into `line` (cleared first), its `\n` or `\r\n` removed,
/// holding at most `limit` bytes of it: a line that reaches `limit` before its
/// newline is returned `Cut` with the rest left in `reader`.
async fn read_line_capped<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<Line>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    line.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if line.is_empty() {
                Line::End
            } else {
                Line::Whole
            });
        }
        let room = limit - line.len();
        let (used, outcome) = match available.iter().position(|&b| b == b'\n') {
            Some(end) if end < room => {
                line.extend(available.iter().take(end));
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                (end + 1, Some(Line::Whole))
            }
            _ if available.len() >= room => {
                line.extend(available.iter().take(room));
                (room, Some(Line::Cut))
            }
            _ => {
                line.extend_from_slice(available);
                (available.len(), None)
            }
        };
        reader.consume(used);
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
    }
}

/// Read and drop the rest of a line, up to and including its newline.
async fn skip_line<R>(reader: &mut R) -> std::io::Result<()>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(());
        }
        let (used, done) = available
            .iter()
            .position(|&b| b == b'\n')
            .map_or((available.len(), false), |end| (end + 1, true));
        reader.consume(used);
        if done {
            return Ok(());
        }
    }
}

/// Drop a character that a cut split at the end of `bytes`.
fn cut_to_char(bytes: &mut Vec<u8>) {
    if let Err(e) = std::str::from_utf8(bytes)
        && e.error_len().is_none()
    {
        bytes.truncate(e.valid_up_to());
    }
}

/// `text` cut to `max` characters.
fn cut_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The stderr lines that report an error, such as Codex's
/// `ERROR: unexpected status 401 Unauthorized`.
fn error_lines(stderr_tail: &[String]) -> Vec<&str> {
    stderr_tail
        .iter()
        .map(|line| line.trim())
        .filter(|line| {
            line.contains("ERROR")
                || line
                    .get(..5)
                    .is_some_and(|head| head.eq_ignore_ascii_case("error"))
        })
        .collect()
}

/// Surface the child's own stderr when it has any -- "Invalid API key, please
/// run /login" is actionable in a way that a generic failure string is not.
/// Its error lines say why when there are any; the rest is context. Cut like
/// every other provider error, since the overlay shows it whole.
fn cli_failure_message(label: &str, stderr_tail: &[String]) -> String {
    let errors = error_lines(stderr_tail);
    let reason = if errors.is_empty() {
        stderr_tail.join(" / ")
    } else {
        errors.join(" / ")
    };
    if reason.is_empty() {
        format!("{label} CLI produced no output. Check the launcher log.")
    } else {
        cut_chars(
            &format!("{label} CLI failed: {reason}"),
            super::gemini::MAX_ERROR_MESSAGE_CHARS,
        )
    }
}

/// The error for a WSL run whose script never printed its start marker: the
/// shell, or whatever the profile started in its place, never reached the
/// CLI. A profile that starts a shell which cannot run a POSIX script (fish)
/// ends here, as does one that reads stdin. The shell's output is cut, the
/// hint after it is not. A script that could not map a file's Windows path
/// (`WSL_PATH_UNMAPPED`) says so instead: the profile is not the cause.
fn not_started_message(label: &str, stderr_tail: &[String]) -> String {
    if stderr_tail
        .iter()
        .any(|line| line.trim_end().ends_with(WSL_PATH_UNMAPPED))
    {
        return format!(
            "{label} did not start in WSL: the Windows drive is not reachable from WSL, so \
             the question could not be handed to the CLI. Sage needs WSL to mount the Windows \
             drives (automount in /etc/wsl.conf)."
        );
    }
    let reason = if stderr_tail.is_empty() {
        "the WSL shell ended before it ran the CLI".to_owned()
    } else {
        cut_chars(
            &stderr_tail.join(" / "),
            super::gemini::MAX_ERROR_MESSAGE_CHARS,
        )
    };
    format!(
        "{label} did not start in WSL: {reason}. If ~/.profile or ~/.bashrc starts \
         another shell (such as exec fish) or reads input, do that only in a terminal, \
         for example behind [ -t 0 ] &&."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for `wsl.exe`, used by every `wsl_exec` on this thread.
    #[derive(Clone)]
    pub(super) struct FakeWsl {
        pub(super) program: std::path::PathBuf,
        /// The script `program` runs, when it is a shell.
        pub(super) script: Option<std::path::PathBuf>,
        /// This fake's own `wsl_home` cache, so tests never share one.
        pub(super) home: &'static std::sync::OnceLock<String>,
    }

    thread_local! {
        static FAKE_WSL: std::cell::RefCell<Option<FakeWsl>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn fake_wsl() -> Option<FakeWsl> {
        FAKE_WSL.with_borrow(Clone::clone)
    }

    /// Puts a fake `wsl.exe` in place on this thread until it is dropped.
    struct FakeWslGuard(std::path::PathBuf);

    impl Drop for FakeWslGuard {
        fn drop(&mut self) {
            FAKE_WSL.set(None);
            crate::util::log_if_err("remove the fake wsl", std::fs::remove_dir_all(&self.0));
        }
    }

    /// Install a fake `wsl.exe` running `unix` (a `sh` script body) or, on
    /// Windows, `windows` (a batch file body). It gets the real arguments,
    /// starting with `--exec`.
    fn install_fake_wsl(name: &str, unix: &str, windows: &str) -> FakeWslGuard {
        let dir = fresh_dir(&format!("fake-wsl-{name}"));
        // On Unix the script is run by `sh`, not executed itself: a child
        // another test forks while this file is open for writing keeps it
        // open until it execs, and executing the file then fails with
        // "Text file busy".
        let (program, script) = if cfg!(windows) {
            let path = dir.join("wsl.cmd");
            std::fs::write(&path, format!("@echo off\r\n{windows}\r\n")).unwrap();
            (path, None)
        } else {
            let path = dir.join("wsl.exe");
            std::fs::write(&path, format!("#!/bin/sh\n{unix}\n")).unwrap();
            ("/bin/sh".into(), Some(path))
        };
        let home = Box::leak(Box::new(std::sync::OnceLock::new()));
        FAKE_WSL.set(Some(FakeWsl {
            program,
            script,
            home,
        }));
        FakeWslGuard(dir)
    }

    /// Names no install has, so the native probes find nothing.
    const NO_CLAUDE: &str = "aigc-test-absent-claude";
    const NO_CODEX: &str = "aigc-test-absent-codex";
    const SCALED_DETECT: std::time::Duration = std::time::Duration::from_secs(1);

    /// What a WSL that has the CLI prints for its version probe.
    const UNIX_VERSION_ANSWER: &str = "printf '\\n%s\\ncli 1.0\\n' __AIGC_STREAM_BEGIN__";
    const WINDOWS_VERSION_ANSWER: &str = "echo.\r\necho __AIGC_STREAM_BEGIN__\r\necho cli 1.0";

    #[test]
    fn detection_gives_up_on_a_wsl_that_never_answers() {
        let _fake = install_fake_wsl("hang", "exec sleep 10", "ping -n 11 127.0.0.1 >nul");
        let started = std::time::Instant::now();
        let cfg = detect_named(NO_CLAUDE, NO_CODEX, SCALED_DETECT, None);
        let took = started.elapsed();
        println!("{cfg:?} after {took:?}");
        assert!(
            took < std::time::Duration::from_secs(5),
            "detection waited {took:?}"
        );
        assert_eq!(
            (cfg.claude, cfg.codex),
            (CliMode::Unavailable, CliMode::Unavailable)
        );
    }

    /// A Codex on the Windows PATH works in the app's own local data folder,
    /// never in a shared temp folder; without that folder it has no workdir.
    #[test]
    fn the_native_codex_workdir_is_in_the_app_data_folder() {
        let profile = fresh_dir("local-data");
        let workdir = ensure_codex_workdir(CliMode::Native, SCALED_DETECT, Some(&profile));
        let made = workdir
            .as_deref()
            .is_some_and(|dir| std::path::Path::new(dir).is_dir());
        let unknown = ensure_codex_workdir(CliMode::Native, SCALED_DETECT, None);
        std::fs::remove_dir_all(&profile).unwrap();
        println!("workdir {workdir:?}, without the folder {unknown:?}");
        assert_eq!(
            workdir.map(std::path::PathBuf::from),
            Some(profile.join(CODEX_WORKDIR))
        );
        assert!(made, "the workdir was not created");
        assert_eq!(unknown, None);
    }

    /// WSL fails the first home lookup (the VM still starting), then answers.
    #[test]
    fn a_failed_wsl_home_lookup_is_tried_again() {
        let dir = fresh_dir("home-retry-count");
        let count = dir.join("asked");
        let unix = format!(
            "[ -f '{c}' ] && {{ echo /srv/probe-home; exit 0; }}\n: > '{c}'; echo 'wsl: VM starting' >&2; exit 1",
            c = count.display()
        );
        let windows = format!(
            "if exist \"{c}\" (echo /srv/probe-home\r\nexit /b 0)\r\ntype nul > \"{c}\"\r\necho wsl: VM starting 1>&2\r\nexit /b 1",
            c = count.display()
        );
        let _fake = install_fake_wsl("home-retry", &unix, &windows);
        let first = wsl_home(SCALED_DETECT);
        let second = wsl_home(SCALED_DETECT);
        let workdir = ensure_codex_workdir(CliMode::Wsl, SCALED_DETECT, None);
        std::fs::remove_dir_all(&dir).unwrap();
        println!("first {first:?}, second {second:?}, workdir {workdir:?}");
        assert_eq!(first, None);
        assert_eq!(second, Some("/srv/probe-home"));
        assert_eq!(
            workdir.as_deref(),
            Some("/srv/probe-home/.cache/aigc-codex-workdir")
        );
    }

    /// The version probe answers; only the home lookup (`--exec sh ...`) hangs.
    #[test]
    fn detection_gives_up_on_a_wsl_home_lookup_that_hangs() {
        let unix = format!("[ \"$2\" = sh ] && exec sleep 10\n{UNIX_VERSION_ANSWER}");
        let windows = format!(
            "if \"%2\"==\"sh\" (ping -n 11 127.0.0.1 >nul\r\nexit /b 1)\r\n{WINDOWS_VERSION_ANSWER}"
        );
        let _fake = install_fake_wsl("home-hang", &unix, &windows);
        let started = std::time::Instant::now();
        let cfg = detect_named(NO_CLAUDE, NO_CODEX, SCALED_DETECT, None);
        let took = started.elapsed();
        println!("{cfg:?} after {took:?}");
        assert!(
            took < std::time::Duration::from_secs(5),
            "detection waited {took:?}"
        );
        assert_eq!(cfg.claude, CliMode::Wsl);
        assert_eq!(cfg.codex, CliMode::Unavailable, "Codex without a workdir");
    }

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_owned(),
            content: content.to_owned(),
        }
    }

    // ---------------- WSL spawning ----------------

    #[test]
    fn every_wsl_spawn_uses_the_builder() {
        let needle = concat!("Command::new(", "\"wsl.exe\")");
        let (files, count) = crate::util::count_in_sources(needle, None);
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(count, 1, "every wsl.exe child must be built by wsl_exec");
    }

    #[test]
    fn wsl_bash_passes_script_without_a_second_shell() {
        let cmd = wsl_bash();
        assert_eq!(cmd.get_program(), "wsl.exe");
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["--exec", "bash", "--noediting", "-lis"]
        );
        assert_eq!(wsl_stdin("S"), "\nunset HISTFILE; S\n");
    }

    #[test]
    fn version_probe_escapes_the_name() {
        assert_eq!(
            version_script("x; y"),
            format!("{}'x; y' --version </dev/null", start_marker())
        );
    }

    /// A WSL stand-in that runs the real `bash` of this host: `wsl.exe --exec
    /// <program...>` runs the program with `HOME` at a fresh directory whose
    /// `.profile` is `profile`, and a `bin` first on PATH holding `wslpath`
    /// (prints the path it is given) and the CLIs in `clis` (name, `sh` body).
    /// Returns the guard and the fake's directory, which holds an empty
    /// `files` folder for the request's temp files.
    #[cfg(unix)]
    fn install_profiled_wsl(
        name: &str,
        profile: &str,
        clis: &[(&str, &str)],
    ) -> (FakeWslGuard, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let root =
            std::env::temp_dir().join(format!("aigc-test-fake-wsl-{name}-{}", std::process::id()));
        let wsl = format!(
            "shift\nHOME='{r}/home' PATH='{r}/bin':/usr/bin:/bin exec \"$@\"",
            r = root.display()
        );
        let guard = install_fake_wsl(name, &wsl, "");
        let mut files = vec![
            ("home/.profile".to_owned(), profile.to_owned()),
            (
                "bin/wslpath".to_owned(),
                "#!/bin/sh\nprintf '%s\\n' \"$2\"".to_owned(),
            ),
        ];
        for (cli, body) in clis {
            files.push((format!("bin/{cli}"), format!("#!/bin/sh\n{body}\n")));
        }
        for (file, body) in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir(root.join("files")).unwrap();
        (guard, root)
    }

    /// A fake `codex` that keeps what it reads on stdin in `~/capture`, then answers.
    #[cfg(unix)]
    const CAPTURING_CODEX: (&str, &str) = ("codex", "cat > \"$HOME/capture\"\necho 'The answer.'");

    /// Ask the fake WSL's Codex one question; returns the result and the answer.
    #[cfg(unix)]
    async fn ask_codex_in_wsl(root: &std::path::Path) -> (Result<(), String>, String) {
        let cfg = CliConfig {
            claude: CliMode::Unavailable,
            codex: CliMode::Wsl,
            codex_workdir: "/work".to_owned(),
        };
        let messages = [msg("user", "QUESTION-TEXT")];
        let mut chunks = Vec::new();
        let result = stream_codex(
            &cfg,
            "SYSTEM-TEXT",
            &messages,
            Some(&root.join("files")),
            None,
            |text| {
                chunks.push(text);
                Ok(())
            },
        )
        .await;
        (result, chunks.concat())
    }

    /// The temp files a request left in the fake's `files` folder.
    #[cfg(unix)]
    fn left_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(root.join("files"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    }

    /// A profile that replaces bash with `sh` hands it the rest of stdin: the
    /// script must still run there, and the CLI must read the question from
    /// its file -- never the shell, which would run each line as a command.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_profile_that_starts_another_shell_still_runs_the_cli() {
        let (_fake, root) = install_profiled_wsl("exec-sh", "exec sh\n", &[CAPTURING_CODEX]);
        let (result, answer) = ask_codex_in_wsl(&root).await;
        let captured = std::fs::read_to_string(root.join("home/capture")).unwrap_or_default();
        println!("{result:?} {answer:?}\ncaptured {captured:?}");
        assert_eq!(result, Ok(()));
        assert_eq!(answer.trim(), "The answer.");
        assert!(captured.contains("QUESTION-TEXT"), "{captured:?}");
        assert!(captured.contains("SYSTEM-TEXT"), "{captured:?}");
        assert_eq!(left_files(&root), Vec::<std::path::PathBuf>::new());
    }

    /// A profile that reads stdin gets the script, never the chat.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_profile_that_reads_stdin_never_sees_the_chat() {
        let (_fake, root) =
            install_profiled_wsl("reads-stdin", "cat > \"$HOME/seen\"\n", &[CAPTURING_CODEX]);
        let (result, answer) = ask_codex_in_wsl(&root).await;
        let seen = std::fs::read_to_string(root.join("home/seen")).unwrap_or_default();
        println!("{result:?} {answer:?}\nseen {seen:?}");
        assert!(!seen.contains("QUESTION-TEXT"), "{seen:?}");
        assert!(!seen.contains("SYSTEM-TEXT"), "{seen:?}");
        let error = result.unwrap_err();
        assert!(error.contains("did not start in WSL"), "{error}");
        assert_eq!(left_files(&root), Vec::<std::path::PathBuf>::new());
    }

    /// A WSL that cannot map the prompt file's Windows path (its drive is not
    /// mounted): the error names that, not the profile, and the CLI never runs.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unmapped_windows_drive_is_named_as_the_cause() {
        let unmapped = (
            "wslpath",
            "echo \"wslpath: $2: Result not representable\" >&2\nexit 1",
        );
        let (_fake, root) = install_profiled_wsl("unmapped", "", &[CAPTURING_CODEX, unmapped]);
        let (result, answer) = ask_codex_in_wsl(&root).await;
        println!("{result:?} {answer:?}");
        let error = result.unwrap_err();
        assert!(
            error.contains("Windows drive is not reachable from WSL"),
            "{error}"
        );
        assert!(
            !error.contains("~/.profile"),
            "the profile is blamed: {error}"
        );
        assert!(!root.join("home/capture").exists(), "the CLI ran");
        assert_eq!(left_files(&root), Vec::<std::path::PathBuf>::new());
    }

    /// A profile that sets the terminal title prints it without a newline, so
    /// it shares a line with the start marker; the answer must still arrive,
    /// and the CLI must still be found.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_profile_printing_without_a_newline_keeps_the_answer() {
        const VERSIONED: (&str, &str) = ("aigc-test-versioned-cli", "echo 'cli 1.0'");
        let (_fake, root) = install_profiled_wsl(
            "title",
            "printf '\\033]0;x\\007'\n",
            &[CAPTURING_CODEX, VERSIONED],
        );
        let (result, answer) = ask_codex_in_wsl(&root).await;
        let found = detect_cli(VERSIONED.0, std::time::Duration::from_secs(10));
        println!("{result:?} {answer:?}, detected {found:?}");
        assert_eq!(result, Ok(()));
        assert_eq!(answer.trim(), "The answer.");
        assert_eq!(found, CliMode::Wsl);
    }

    /// Only a CLI that prints its own version line counts as installed: a
    /// shell that ran nothing, or a CLI that printed nothing, also ends with 0.
    #[cfg(unix)]
    #[test]
    fn detection_needs_the_version_line() {
        const VERSIONED: &str = "aigc-test-versioned-cli";
        const SILENT: &str = "aigc-test-silent-cli";
        let limit = std::time::Duration::from_secs(10);
        let clis = [(VERSIONED, "echo 'cli 1.0'"), (SILENT, "exit 0")];
        for profile in ["exec sh\n", ""] {
            let (_fake, _) = install_profiled_wsl("detect", profile, &clis);
            let found = [NO_CODEX, SILENT, VERSIONED].map(|name| detect_cli(name, limit));
            println!("profile {profile:?}: {found:?}");
            assert_eq!(
                found,
                [CliMode::Unavailable, CliMode::Unavailable, CliMode::Wsl],
                "profile {profile:?}"
            );
        }
    }

    /// Every input must come back byte for byte: anything else means a shell
    /// other than the `bash` we start parsed the script.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs WSL"]
    fn wsl_round_trips_hostile_text() {
        let inputs = [
            "$(echo INJECTED)",
            "`echo INJECTED`",
            "$HOME and $USER",
            "say \"hi\"",
            "it's",
            r"a\\b",
            r"ends with \",
            "line one\n$(echo INJECTED)",
            "%PATH% !bang!",
            "caf\u{e9} \u{65e5}\u{672c}",
        ];
        let marker = format!("{WSL_SENTINEL}\n");
        for input in inputs {
            let script = format!(
                "printf '%s\\n' {WSL_SENTINEL}; printf '%s' {}",
                shell_escape(input)
            );
            let out = output_within(
                &mut wsl_bash(),
                &wsl_stdin(&script),
                std::time::Duration::from_secs(60),
            )
            .unwrap();
            let stdout = String::from_utf8(out.stdout).unwrap();
            let result = stdout
                .strip_prefix(&marker)
                .or_else(|| {
                    stdout
                        .split_once(&format!("\n{marker}"))
                        .map(|(_, rest)| rest)
                })
                .unwrap();
            println!("input  {input:?}\nresult {result:?}");
            assert_eq!(result, input);
        }
    }

    /// The real WSL runs a CLI (`cat`) on a prompt file under a Windows path:
    /// the prompt comes back unchanged and the file is gone afterwards.
    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "needs WSL"]
    async fn wsl_hands_the_prompt_file_to_the_cli() {
        let dir = fresh_dir("wsl-prompt-file");
        let prompt = "line one\n$(echo INJECTED) it's `x`\n";
        let mut chunks = Vec::new();
        let result = run_in_wsl(
            "cat",
            None,
            Some(&dir),
            CliRun::plain("Test", prompt.to_owned()),
            |text| {
                chunks.push(text);
                Ok(())
            },
            parse_codex_line,
        )
        .await;
        let left = std::fs::read_dir(&dir).unwrap().count();
        std::fs::remove_dir_all(&dir).unwrap();
        println!("{result:?} {chunks:?}, {left} file(s) left");
        assert_eq!(result, Ok(()));
        assert_eq!(chunks.concat(), prompt);
        assert_eq!(left, 0);
    }

    // ---------------- npm installs on Windows ----------------

    /// The `.cmd` file npm writes for a CLI whose entry point is a script,
    /// with an invented package.
    #[cfg(windows)]
    const NPM_SHIM: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n\
        :start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  \
        SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  \
        SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || \
        title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@example\\fakecli\\bin\\fakecli.js\" %*\r\n";

    /// Arguments `cmd.exe` would act on, and an empty one (`--tools ''`).
    /// Claude's system prompt is one argument and spans many lines.
    #[cfg(windows)]
    fn hostile_args(marker: &std::path::Path) -> Vec<String> {
        let marker = marker.display();
        vec![
            format!("a & echo INJECTED > \"{marker}\""),
            format!("b | echo INJECTED > \"{marker}\""),
            format!("c ^& echo INJECTED > \"{marker}\""),
            "%PATH% %COMSPEC% !bang!".to_owned(),
            String::new(),
            r#"say "hi" \" and \\"#.to_owned(),
            format!("line one\nline two & echo INJECTED > \"{marker}\""),
            format!("line one\r\n\"& echo INJECTED > \"{marker}\""),
        ]
    }

    #[cfg(windows)]
    #[test]
    fn an_npm_installed_cli_is_found_and_runs_without_cmd_exe() {
        let dir = fresh_dir("npm-shim");
        let bin = dir.join(r"node_modules\@example\fakecli\bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("fakecli.js"),
            "process.stdout.write(JSON.stringify(process.argv.slice(2)));",
        )
        .unwrap();
        std::fs::write(dir.join("fakecli.cmd"), NPM_SHIM).unwrap();
        let marker = dir.join("INJECTED");
        let args = hostile_args(&marker);

        let mut cmd = native_command_in("fakecli", dir.as_os_str());
        let program = std::path::Path::new(cmd.get_program()).to_owned();
        let out = cmd.args(&args).output();
        println!("program {program:?}, output {out:?}");
        let out = out.expect("the npm-installed CLI runs");
        let seen: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
        let injected = marker.exists();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(seen, args, "every argument arrives as written");
        assert!(!injected, "an argument ran a command");
        assert!(
            !program
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe")),
            "the CLI ran through cmd.exe"
        );
    }

    #[cfg(windows)]
    #[test]
    fn npm_shims_resolve_like_the_batch_file() {
        let dir = fresh_dir("npm-forms");
        let path = |dirs: &[&std::path::Path]| std::env::join_paths(dirs).unwrap();
        let program = |cmd: &std::process::Command| std::path::PathBuf::from(cmd.get_program());
        let args = |cmd: &std::process::Command| {
            cmd.get_args()
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>()
        };

        // A script run by `node`: the one beside the batch file comes first.
        let script = dir.join(r"node_modules\@example\fakecli\bin\fakecli.js");
        std::fs::write(dir.join("fakecli.cmd"), NPM_SHIM).unwrap();
        let cmd = native_command_in("fakecli", &path(&[&dir]));
        assert_eq!(
            (program(&cmd), args(&cmd)),
            ("node".into(), vec![script.clone()])
        );
        std::fs::write(dir.join("node.exe"), b"").unwrap();
        let cmd = native_command_in("fakecli", &path(&[&dir]));
        assert_eq!(
            (program(&cmd), args(&cmd)),
            (dir.join("node.exe"), vec![script])
        );

        // A program the batch file runs itself.
        let exe = dir.join(r"node_modules\@example\exetool\bin\exetool.exe");
        std::fs::write(
            dir.join("exetool.cmd"),
            "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\n\
             SETLOCAL\r\nCALL :find_dp0\r\n\"%dp0%\\node_modules\\@example\\exetool\\bin\\exetool.exe\"   %*\r\n",
        )
        .unwrap();
        let cmd = native_command_in("exetool", &path(&[&dir]));
        assert_eq!((program(&cmd), args(&cmd)), (exe, vec![]));

        // PATH order decides: an `.exe` in an earlier folder wins.
        let first = dir.join("first");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::write(first.join("fakecli.exe"), b"").unwrap();
        let cmd = native_command_in("fakecli", &path(&[&first, &dir]));
        assert_eq!((program(&cmd), args(&cmd)), ("fakecli".into(), vec![]));

        // A batch file in another shape is not run.
        std::fs::write(dir.join("other.cmd"), "@echo off\r\nsomething.exe %*\r\n").unwrap();
        let cmd = native_command_in("other", &path(&[&dir]));
        assert_eq!((program(&cmd), args(&cmd)), ("other".into(), vec![]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---------------- shell_escape ----------------

    #[test]
    fn shell_escape_wraps_in_single_quotes() {
        assert_eq!(shell_escape("plain"), "'plain'");
    }

    #[test]
    fn shell_escape_preserves_spaces_and_special_chars() {
        assert_eq!(shell_escape("a b $c & d"), "'a b $c & d'");
    }

    #[test]
    fn shell_escape_escapes_inner_single_quote() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_escape_handles_empty_string() {
        assert_eq!(shell_escape(""), "''");
    }

    // ---------------- validate_model_name ----------------

    #[test]
    fn validate_model_name_accepts_typical_ids() {
        for ok in [
            "gemini-2.5-flash",
            "claude-haiku-4-5",
            "gpt-4o",
            "model_v2",
            "Some.Model.With.Dots",
            "a",
        ] {
            assert!(validate_model_name(ok).is_ok(), "should accept: {ok}");
        }
    }

    #[test]
    fn validate_model_name_rejects_empty_and_oversize() {
        assert!(validate_model_name("").is_err());
        let oversize = "a".repeat(129);
        assert!(validate_model_name(&oversize).is_err());
    }

    #[test]
    fn validate_model_name_rejects_path_traversal() {
        for bad in [
            "../foo", "foo/bar", "foo\\bar", "foo bar", "foo:bar", "foo$",
        ] {
            assert!(validate_model_name(bad).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn validate_model_name_rejects_non_ascii() {
        assert!(validate_model_name("mod\u{e8}le").is_err());
    }

    // ---------------- build_codex_input ----------------

    #[test]
    fn codex_input_omits_system_prompt_when_empty() {
        let out = build_codex_input("", &[msg("user", "hello")]);
        assert_eq!(out, "[user]: hello\n");
    }

    #[test]
    fn codex_input_includes_system_prompt_with_blank_line() {
        let out = build_codex_input("Be terse.", &[msg("user", "hi")]);
        assert_eq!(out, "Be terse.\n\n[user]: hi\n");
    }

    #[test]
    fn codex_input_concatenates_messages_in_order() {
        let out = build_codex_input(
            "",
            &[msg("user", "q1"), msg("assistant", "a1"), msg("user", "q2")],
        );
        assert_eq!(out, "[user]: q1\n[assistant]: a1\n[user]: q2\n");
    }

    #[test]
    fn system_prompt_reaches_the_clis_unchanged() {
        let prompt =
            crate::ai::prompt::assemble(crate::ai::prompt::PromptKind::Chat { hints: true });
        println!("system prompt: {} bytes", prompt.len());
        let codex = build_codex_input(&prompt, &[msg("user", "Where now?")]);
        assert!(
            codex.starts_with(&format!("{prompt}\n\n")),
            "Codex input does not open with the prompt"
        );
        let args = claude_args(DEFAULT_CLAUDE_MODEL, &prompt);
        let flags: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "--system-prompt")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            flags.len(),
            1,
            "--system-prompt given {} times",
            flags.len()
        );
        assert_eq!(args.get(flags[0] + 1), Some(&prompt));
        let script = claude_wsl_script(DEFAULT_CLAUDE_MODEL, &prompt);
        assert!(
            script.contains(&shell_escape(&prompt)),
            "the WSL script does not carry the quoted prompt"
        );
    }

    // ---------------- build_claude_input ----------------

    #[test]
    fn claude_input_emits_one_ndjson_line_terminated_by_newline() {
        let out = build_claude_input(&[msg("user", "hello")], None);
        assert!(out.ends_with('\n'));
        assert_eq!(out.matches('\n').count(), 1);
    }

    #[test]
    fn claude_input_concatenates_history_as_single_user_turn() {
        let out = build_claude_input(
            &[msg("user", "q1"), msg("assistant", "a1"), msg("user", "q2")],
            None,
        );
        let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        let parts = v["message"]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(
            parts[0]["text"].as_str().unwrap(),
            "[user]: q1\n[assistant]: a1\n[user]: q2"
        );
    }

    #[test]
    fn claude_input_appends_image_part_when_screenshot_present() {
        let out = build_claude_input(&[msg("user", "look")], Some("AAAAFAKE=="));
        let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        let parts = v["message"]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["type"], "image");
        assert_eq!(parts[1]["source"]["type"], "base64");
        assert_eq!(parts[1]["source"]["media_type"], "image/png");
        assert_eq!(parts[1]["source"]["data"], "AAAAFAKE==");
    }

    #[test]
    fn claude_input_omits_image_when_no_screenshot() {
        let out = build_claude_input(&[msg("user", "hi")], None);
        let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        let parts = v["message"]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
    }

    // ---------------- parse_claude_line ----------------

    #[test]
    fn parse_claude_line_extracts_text_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"PONG"}}}"#;
        assert_eq!(
            parse_claude_line(line),
            Some(Parsed::Text("PONG".to_owned()))
        );
    }

    #[test]
    fn parse_claude_line_ignores_non_text_deltas() {
        let start = r#"{"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant"}}}"#;
        let block = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#;
        assert_eq!(parse_claude_line(start), None);
        assert_eq!(parse_claude_line(block), None);
    }

    #[test]
    fn parse_claude_line_ignores_system_and_assistant_frames() {
        let system = r#"{"type":"system","subtype":"init","session_id":"x"}"#;
        let assistant = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"PONG"}]}}"#;
        assert_eq!(parse_claude_line(system), None);
        assert_eq!(parse_claude_line(assistant), None);
    }

    #[test]
    fn parse_claude_line_treats_successful_result_as_stream_end() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"PONG"}"#;
        assert_eq!(parse_claude_line(line), None);
    }

    #[test]
    fn parse_claude_line_surfaces_error_result_message() {
        let line = r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: 529 overloaded"}"#;
        assert_eq!(
            parse_claude_line(line),
            Some(Parsed::Error("API Error: 529 overloaded".to_owned()))
        );
    }

    #[test]
    fn parse_claude_line_uses_fallback_when_error_result_has_no_message() {
        let line = r#"{"type":"result","is_error":true}"#;
        assert_eq!(
            parse_claude_line(line),
            Some(Parsed::Error(
                "Claude CLI returned an error without a message".to_owned()
            ))
        );
    }

    #[test]
    fn claude_result_error_table() {
        use serde_json::json;
        const FALLBACK: &str = "Claude CLI returned an error without a message";
        let row = |frame: serde_json::Value| {
            let got = claude_result_error(&frame);
            let shown = frame.to_string().chars().take(100).collect::<String>();
            let message = got
                .as_deref()
                .map(|got| got.chars().take(100).collect::<String>());
            println!("{shown} -> {message:?}");
            got
        };
        assert_eq!(
            row(json!({"type":"result","subtype":"success","is_error":true,"result":"X"})),
            Some("X".to_owned())
        );
        assert_eq!(
            row(json!({"type":"result","subtype":"error_during_execution","errors":["A","B"]})),
            Some("A; B".to_owned())
        );
        let http = row(json!({"type":"result","is_error":true,"api_error_status":529})).unwrap();
        assert!(http.contains("HTTP 529"), "{http}");
        let turns = row(json!({"type":"result","subtype":"error_max_turns"})).unwrap();
        assert!(turns.contains("error_max_turns"), "{turns}");
        assert_eq!(
            row(json!({"type":"result","is_error":true})),
            Some(FALLBACK.to_owned())
        );
        let long = row(json!({"type":"result","is_error":true,"result":"\u{e9}".repeat(1000)}));
        assert_eq!(long.map(|message| message.chars().count()), Some(300));
        assert_eq!(
            row(json!({"type":"result","subtype":"success","is_error":false,"result":"PONG"})),
            None
        );
        assert_eq!(
            row(json!({"type":"result","is_error":true,"result":"  ","errors":["  ",7,"B"]})),
            Some("B".to_owned())
        );
        assert_eq!(
            row(json!({"type":"result","subtype":"success","is_error":true})),
            Some(FALLBACK.to_owned())
        );
    }

    #[test]
    fn no_generic_claude_error_text() {
        let needle = concat!("Unknown Claude", " CLI error");
        let (files, count) = crate::util::count_in_sources(needle, None);
        println!("scanned {files} files, found {count} occurrence(s) of {needle}");
        assert!(files > 0, "the source scan found no files");
        assert_eq!(count, 0, "a Claude error must carry the CLI's own text");
    }

    #[test]
    fn parse_claude_line_skips_malformed_or_empty_lines() {
        assert_eq!(parse_claude_line("not json"), None);
        assert_eq!(parse_claude_line(""), None);
    }

    // ---------------- Codex screenshot file ----------------

    /// A fresh, empty directory for one test.
    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aigc-test-{name}-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn temp_shot_is_deleted_on_drop() {
        let dir = fresh_dir("drop");
        let path = shot_path(&dir);
        let shot = TempFile::create(path.clone(), b"PNGDATA").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"PNGDATA");
        drop(shot);
        assert!(!path.exists(), "the screenshot outlived its guard");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn temp_shot_never_writes_through_an_existing_file() {
        let dir = fresh_dir("existing");
        let path = dir.join("taken.png");
        std::fs::write(&path, b"ORIGINAL").unwrap();
        TempFile::create(path.clone(), b"NEW").unwrap_err();
        assert_eq!(std::fs::read(&path).unwrap(), b"ORIGINAL");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_shot_failure_is_an_error() {
        let no_dir = codex_shot(None, Some(b"PNG"));
        println!("png, no folder: {no_dir:?}");
        assert_eq!(no_dir.unwrap_err(), CODEX_SHOT_FAILED);

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let missing = std::env::temp_dir().join(format!("aigc-test-missing-{nanos}"));
        assert!(!missing.exists());
        let unwritable = codex_shot(Some(&missing), Some(b"PNG"));
        println!("png, missing folder: {unwritable:?}");
        assert_eq!(unwritable.unwrap_err(), CODEX_SHOT_FAILED);

        let dir = fresh_dir("codex-shot");
        let none = codex_shot(Some(&dir), None);
        println!("folder, no png: {none:?}");
        assert!(none.unwrap().is_none());

        let written = codex_shot(Some(&dir), Some(b"PNG")).unwrap().unwrap();
        println!("folder and png: {written:?}");
        assert_eq!(std::fs::read(&written.path).unwrap(), b"PNG");
        let path = written.path.clone();
        drop(written);
        assert!(!path.exists(), "the screenshot outlived its guard");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shot_path_names_a_file_in_the_directory() {
        let dir = std::path::Path::new("shots-dir");
        let path = shot_path(dir);
        assert_eq!(path.parent(), Some(dir));
        let name = path.file_name().unwrap().to_str().unwrap();
        println!("{name}");
        let (pid, nanos) = name
            .strip_prefix("aigc-shot-")
            .and_then(|rest| rest.strip_suffix(".png"))
            .and_then(|middle| middle.split_once('-'))
            .unwrap();
        let digits =
            |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
        assert!(digits(pid), "process id {pid:?}");
        assert!(digits(nanos), "timestamp {nanos:?}");
    }

    #[test]
    fn sweep_removes_leftover_shots() {
        let dir = fresh_dir("sweep");
        for name in ["a.png", "b.png", "c.png"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(sweep_shots(&dir), 3);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(sweep_shots(&dir), 0, "a missing directory is not an error");
    }

    #[test]
    fn codex_args_add_one_image_token() {
        let plain = codex_args("W", None);
        assert_eq!(
            plain,
            [
                "-a",
                "never",
                "-s",
                "read-only",
                "--disable",
                "shell_tool",
                "-C",
                "W",
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-user-config"
            ]
        );
        assert!(plain.iter().all(|arg| !arg.contains("--image")));

        let with_image = codex_args("W", Some(r"C:\shots\a b.png"));
        let (last, before) = with_image.split_last().unwrap();
        assert_eq!(before, plain.as_slice());
        assert_eq!(last, r"--image=C:\shots\a b.png");
        assert_eq!(
            with_image
                .iter()
                .filter(|arg| arg.contains("--image"))
                .count(),
            1
        );
    }

    #[test]
    fn codex_wsl_script_hands_the_image_over() {
        assert_eq!(
            codex_wsl_script("/srv/work dir"),
            "codex -a never -s read-only --disable shell_tool -C '/srv/work dir' exec --skip-git-repo-check --ephemeral --ignore-user-config"
        );

        let prompt = r"C:\files\aigc-prompt-1.txt";
        let start = start_marker();
        let unmapped = r"|| { printf '\n%s\n' __AIGC_PATH_UNMAPPED__ >&2; exit 1; };";
        assert_eq!(
            wsl_run_script("cli 'a'", prompt, None),
            format!(
                r#"f=$(wslpath -u 'C:\files\aigc-prompt-1.txt') {unmapped} {start}cli 'a' < "$f"; s=$?; rm -f -- "$f"; exit "$s""#
            )
        );

        let image = r"C:\shots\it's $(x) a.png";
        let script = wsl_run_script("cli 'a'", prompt, Some(image));
        println!("{script}");
        assert!(script.contains(&format!("wslpath -u {}", shell_escape(image))));
        assert_eq!(
            script,
            format!(
                r#"f=$(wslpath -u 'C:\files\aigc-prompt-1.txt') {unmapped} img=$(wslpath -u 'C:\shots\it'\''s $(x) a.png') {unmapped} {start}cli 'a' --image="$img" < "$f"; s=$?; rm -f -- "$f"; exit "$s""#
            )
        );
        assert_eq!(script.matches("--image=").count(), 1);
    }

    #[test]
    fn codex_runs_ephemeral() {
        for image in [None, Some(r"C:\shots\a b.png")] {
            let args = codex_args("W", image);
            println!("{args:?}");
            assert_eq!(
                args.iter().filter(|arg| *arg == "--ephemeral").count(),
                1,
                "{args:?}"
            );
            let skip = args
                .iter()
                .position(|arg| arg == "--skip-git-repo-check")
                .unwrap();
            assert_eq!(
                args.get(skip + 1).map(String::as_str),
                Some("--ephemeral"),
                "{args:?}"
            );

            let script = wsl_run_script(&codex_wsl_script("/srv/work dir"), "P", image);
            println!("{script}");
            assert_eq!(script.matches("--ephemeral").count(), 1, "{script}");
            assert!(
                script.contains("--skip-git-repo-check --ephemeral"),
                "{script}"
            );
        }
    }

    /// Every index at which `flag` appears in `args`.
    fn positions(args: &[String], flag: &str) -> Vec<usize> {
        args.iter()
            .enumerate()
            .filter(|(_, arg)| *arg == flag)
            .map(|(at, _)| at)
            .collect()
    }

    #[test]
    fn claude_children_are_locked_down() {
        let prompt = "You are Sage. It's $(x) `y`";
        let args = claude_args("claude-haiku-4-5", prompt);
        println!("{args:?}");
        let after = |flag: &str| {
            let at = positions(&args, flag);
            assert_eq!(at.len(), 1, "{flag} in {args:?}");
            args.get(at[0] + 1).map(String::as_str)
        };
        for flag in [
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
        ] {
            assert_eq!(positions(&args, flag).len(), 1, "{flag} in {args:?}");
        }
        assert_eq!(after("--permission-mode"), Some("dontAsk"));
        assert_eq!(after("--tools"), Some(""));
        assert_eq!(after("--model"), Some("claude-haiku-4-5"));
        assert_eq!(after("--system-prompt"), Some(prompt));

        let script = claude_wsl_script("claude-haiku-4-5", prompt);
        println!("{script}");
        assert!(script.starts_with("claude "), "{script}");
        for flag in [
            "'--safe-mode'",
            "'--strict-mcp-config'",
            "'--permission-mode' 'dontAsk'",
            "'--tools' ''",
            "'--no-session-persistence'",
        ] {
            assert_eq!(script.matches(flag).count(), 1, "{flag} in {script}");
        }
        assert!(script.contains(&shell_escape(prompt)), "{script}");
    }

    #[test]
    fn codex_children_are_locked_down() {
        for image in [None, Some(r"C:\shots\a b.png")] {
            let args = codex_args("W", image);
            println!("{args:?}");
            let exec = positions(&args, "exec");
            assert_eq!(exec.len(), 1, "{args:?}");
            let disable = positions(&args, "--disable");
            assert_eq!(disable.len(), 1, "{args:?}");
            assert!(disable[0] < exec[0], "--disable must come before exec");
            assert_eq!(
                args.get(disable[0] + 1).map(String::as_str),
                Some("shell_tool")
            );
            let ignore = positions(&args, "--ignore-user-config");
            assert_eq!(ignore.len(), 1, "{args:?}");
            assert!(ignore[0] > exec[0], "--ignore-user-config must follow exec");

            let script = wsl_run_script(&codex_wsl_script("/srv/work dir"), "P", image);
            println!("{script}");
            assert_eq!(script.matches(" exec ").count(), 1, "{script}");
            let (before, after) = script.split_once(" exec ").unwrap();
            assert_eq!(
                before.matches("--disable shell_tool").count(),
                1,
                "{script}"
            );
            assert!(!after.contains("--disable"), "{script}");
            assert_eq!(after.matches("--ignore-user-config").count(), 1, "{script}");
            assert!(!before.contains("--ignore-user-config"), "{script}");
        }
    }

    // ---------------- run_cli line handling ----------------

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_output_keeps_line_breaks() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf 'One.\\nTwo.\\n\\n- a\\n- b\\n'"]);
        let mut chunks = Vec::new();
        let result = run_cli(
            &mut cmd,
            CliRun::plain("Codex", String::new()),
            |text| {
                chunks.push(text);
                Ok(())
            },
            parse_codex_line,
        )
        .await;
        let joined = chunks.concat();
        println!("{joined:?}");
        assert_eq!(result, Ok(()));
        assert_eq!(joined, "One.\nTwo.\n\n- a\n- b\n");
    }

    #[cfg(unix)]
    #[derive(Clone, Default)]
    struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    #[cfg(unix)]
    impl std::io::Write for LogBuf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("log buffer poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    impl LogBuf {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    /// Everything logged while the tests run. A process-wide subscriber,
    /// installed once: a thread-local one misses events whenever a parallel
    /// test registered the call site first (measured: the captured log came
    /// back empty in 3 of 5 full runs). Each test uses a label no other test
    /// uses, which keeps the other tests' lines out of its assertions.
    #[cfg(unix)]
    fn captured_log() -> LogBuf {
        static LOG: std::sync::OnceLock<LogBuf> = std::sync::OnceLock::new();
        LOG.get_or_init(|| {
            let buf = LogBuf::default();
            let writer = buf.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::set_global_default(subscriber).unwrap();
            buf
        })
        .clone()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_cli_keeps_the_chat_out_of_the_log() {
        let buf = captured_log();
        // Codex echoes the whole prompt to stderr, then writes its own error
        // line when the request fails (not logged in, usage limit, network).
        let input = build_codex_input(
            "Answer briefly.",
            &[msg(
                "user",
                "<game_context>\nname: \"Invented Game\"\n</game_context>\n\nWhere is SECRET-QUESTION?\nERROR: my save will not load",
            )],
        );
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "echo user >&2; cat >&2; echo 'ERROR: unexpected status 401 Unauthorized' >&2; exit 1",
        ]);
        let result = run_cli(
            &mut cmd,
            CliRun::plain("Echo", input),
            |_| Ok(()),
            parse_codex_line,
        )
        .await;
        let log = buf.text();
        println!("result: {result:?}");
        println!("log:\n{log}");
        let shown = result.expect_err("a failed CLI is an error");
        for chat_text in ["SECRET-QUESTION", "Invented Game", "my save"] {
            assert!(!log.contains(chat_text), "{chat_text} reached the log");
            assert!(!shown.contains(chat_text), "{chat_text} reached the error");
        }
        assert_eq!(
            shown,
            "Echo CLI failed: ERROR: unexpected status 401 Unauthorized"
        );
        let line = log
            .lines()
            .find(|line| line.contains("Echo CLI failed"))
            .expect("the failure is logged");
        assert!(
            line.contains("exit status: 1")
                && line.ends_with("stderr line(s): ERROR: unexpected status 401 Unauthorized"),
            "the log names how the CLI ended and its own error line: {line}"
        );
    }

    /// A chat line longer than the part of a stderr line that is kept is still
    /// known as the prompt's when the CLI echoes it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_long_echoed_chat_line_stays_out_of_the_error() {
        let long = format!("ERROR SECRET-QUESTION {}", "word ".repeat(2000));
        let input = build_codex_input("Answer briefly.", &[msg("user", &long)]);
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "cat >&2; exit 1"]);
        let result = run_cli(
            &mut cmd,
            CliRun::plain("Longline", input),
            |_| Ok(()),
            parse_codex_line,
        )
        .await;
        println!("result: {result:?}");
        let shown = result.expect_err("a failed CLI is an error");
        assert!(
            !shown.contains("SECRET-QUESTION"),
            "the chat reached the error"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_stderr_is_not_copied_into_the_log() {
        let buf = captured_log();
        // Codex echoes the prompt -- and so the conversation -- to stderr.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo 'user: SECRET-PROMPT' >&2; echo answer"]);
        let result = run_cli(
            &mut cmd,
            CliRun::plain("Probe", String::new()),
            |_| Ok(()),
            parse_codex_line,
        )
        .await;
        let log = buf.text();
        assert_eq!(result, Ok(()));
        assert!(
            !log.contains("SECRET-PROMPT"),
            "stderr text reached the log"
        );
        assert!(
            log.contains("Probe wrote 1 stderr line(s)"),
            "the stderr line count is logged"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn blank_lines_count_toward_the_byte_cap() {
        let script = format!(
            "head -c {} /dev/zero | tr '\\0' '\\n'",
            MAX_STREAM_BYTES + 10
        );
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &script]);
        let result = run_cli(
            &mut cmd,
            CliRun::plain("Codex", String::new()),
            |_| Ok(()),
            parse_codex_line,
        )
        .await;
        println!("{result:?}");
        assert_eq!(
            result,
            Err("Codex response exceeded the size limit.".to_owned())
        );
    }

    /// `script` run by `sh` as a Codex child; the result and how long it took.
    /// A run still going after 20 s counts as hung.
    #[cfg(unix)]
    async fn run_sh(script: &str) -> (Result<(), String>, std::time::Duration) {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            run_cli(
                &mut cmd,
                CliRun::plain("Codex", String::new()),
                |_| Ok(()),
                parse_codex_line,
            ),
        )
        .await
        .unwrap_or_else(|_| Err("still waiting after 20 s".to_owned()));
        (result, started.elapsed())
    }

    /// One 3 MB line, then the child stays alive with both pipes open: the
    /// cap is hit inside the line, and the error does not wait for the child
    /// to close stderr.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_size_cap_holds_within_a_line_and_does_not_wait_for_stderr() {
        let (result, took) =
            run_sh("trap '' PIPE; head -c 3000000 /dev/zero | tr '\\0' a; sleep 8; echo done >&2")
                .await;
        println!("{result:?} after {took:?}");
        assert_eq!(
            result,
            Err("Codex response exceeded the size limit.".to_owned())
        );
        assert!(
            took < std::time::Duration::from_secs(4),
            "it waited {took:?}"
        );
    }

    /// The same line with stderr already closed: only the line's own length
    /// can end the read before its newline comes.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_size_cap_does_not_wait_for_the_end_of_a_long_line() {
        let (result, took) =
            run_sh("exec 2>&-; trap '' PIPE; head -c 3000000 /dev/zero | tr '\\0' a; sleep 8")
                .await;
        println!("{result:?} after {took:?}");
        assert_eq!(
            result,
            Err("Codex response exceeded the size limit.".to_owned())
        );
        assert!(
            took < std::time::Duration::from_secs(4),
            "it waited {took:?}"
        );
    }

    /// A child that fails with one 3 MB error line on stderr: the error
    /// reaching the overlay is cut like every other provider error.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_huge_stderr_line_gives_a_short_failure_message() {
        let (result, _) = run_sh(
            "printf 'ERROR: '>&2; head -c 3000000 /dev/zero | tr '\\0' e >&2; echo >&2; exit 1",
        )
        .await;
        let message = result.unwrap_err();
        println!(
            "{} chars: {}",
            message.chars().count(),
            message.get(..80).unwrap_or(&message)
        );
        assert!(
            message.starts_with("Codex CLI failed: ERROR: eee"),
            "{message:.80}"
        );
        assert!(
            message.chars().count() <= super::super::gemini::MAX_ERROR_MESSAGE_CHARS,
            "{} chars",
            message.chars().count()
        );
    }

    /// A Latin-1 byte on stderr (a localized warning) and on stdout (a banner
    /// before the start marker, a word of the answer): the answer still
    /// arrives, and stderr is read to its end.
    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_utf8_on_either_pipe_keeps_the_answer() {
        let buf = captured_log();
        let script = format!(
            "printf 'warn \\351t\\351\\n' >&2; printf 'Bienvenue \\351\\n'; echo {WSL_SENTINEL}; \
             for i in 1 2; do echo \"working $i\" >&2; done; printf 'caf\\351 ok\\n'"
        );
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &script]);
        let mut run = CliRun::plain("Latin", String::new());
        run.skip_until = Some(WSL_SENTINEL);
        let mut text = String::new();
        let result = run_cli(
            &mut cmd,
            run,
            |chunk| {
                text.push_str(&chunk);
                Ok(())
            },
            parse_codex_line,
        )
        .await;
        println!("{result:?}: {text:?}");
        assert_eq!(result, Ok(()));
        assert_eq!(text, "caf\u{fffd} ok\n");
        assert!(
            buf.text().contains("Latin wrote 3 stderr line(s)"),
            "stderr was not read to its end"
        );
    }

    #[test]
    fn a_long_stderr_tail_is_cut_in_the_failure_messages() {
        let tail = vec!["x".repeat(1000); STDERR_TAIL_LINES];
        let failed = cli_failure_message("Codex", &tail);
        let not_started = not_started_message("Codex", &tail);
        println!(
            "{} / {} chars",
            failed.chars().count(),
            not_started.chars().count()
        );
        assert_eq!(
            failed.chars().count(),
            super::super::gemini::MAX_ERROR_MESSAGE_CHARS
        );
        assert!(not_started.starts_with("Codex did not start in WSL: xxx"));
        assert!(
            not_started.ends_with("behind [ -t 0 ] &&."),
            "the hint is kept"
        );
        assert!(not_started.chars().count() < 2 * super::super::gemini::MAX_ERROR_MESSAGE_CHARS);
    }

    /// `script` run by `sh` with a 1 s idle limit; the result, the text and
    /// how long it took. A run still going after 8 s counts as hung.
    #[cfg(unix)]
    async fn run_with_idle_limit(
        script: &str,
    ) -> (Result<(), String>, String, std::time::Duration) {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        let mut run = CliRun::plain("Idle", String::new());
        run.idle = std::time::Duration::from_millis(1000);
        let started = std::time::Instant::now();
        let mut text = String::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            run_cli(
                &mut cmd,
                run,
                |chunk| {
                    text.push_str(&chunk);
                    Ok(())
                },
                parse_codex_line,
            ),
        )
        .await
        .unwrap_or_else(|_| Err("still waiting after 8 s".to_owned()));
        (result, text, started.elapsed())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_cli_answer_that_keeps_streaming_outlives_the_idle_limit() {
        let (result, text, took) =
            run_with_idle_limit("for i in 1 2 3 4 5 6; do echo \"line $i\"; sleep 0.4; done").await;
        println!("{result:?} after {took:?}: {text:?}");
        assert_eq!(result, Ok(()));
        assert_eq!(text.lines().count(), 6);
    }

    /// Codex prints its progress on stderr and the answer only at the end.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_cli_reporting_progress_on_stderr_is_not_cut() {
        let (result, text, took) = run_with_idle_limit(
            "for i in 1 2 3 4 5 6; do echo \"working $i\" >&2; sleep 0.4; done; echo answer",
        )
        .await;
        println!("{result:?} after {took:?}: {text:?}");
        assert_eq!(result, Ok(()));
        assert_eq!(text, "answer\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_cli_that_goes_silent_times_out() {
        let (result, text, took) = run_with_idle_limit("echo first; exec sleep 6").await;
        println!("{result:?} after {took:?}: {text:?}");
        assert_eq!(result, Err("Request timed out. Try again.".to_owned()));
        assert!(
            took < std::time::Duration::from_secs(3),
            "it waited {took:?}"
        );
    }

    // ---------------- parse_codex_line ----------------

    #[test]
    fn parse_codex_line_emits_plain_text_verbatim() {
        assert_eq!(
            parse_codex_line("PONG"),
            Some(Parsed::Text("PONG\n".to_owned()))
        );
        // A blank line is a paragraph break, not nothing.
        assert_eq!(parse_codex_line(""), Some(Parsed::Text("\n".to_owned())));
    }

    /// Answer lines that happen to be JSON objects, in the shapes that were
    /// once read as protocol frames: a typed object, a `text` field, a
    /// `content` array and a refusal-shaped object.
    const JSON_ANSWER: &str = r#"Put this in the save file:
{"type": "weapon", "name": "Iron Sword", "damage": 12}
{"text": "Press E", "key": "E"}
{"content":[{"type":"output_text","text":"hello"}]}
{"type": "refusal", "content": "sample"}
42
"#;

    #[test]
    fn parse_codex_line_emits_json_answer_lines_verbatim() {
        for line in JSON_ANSWER.lines() {
            assert_eq!(
                parse_codex_line(line),
                Some(Parsed::Text(format!("{line}\n"))),
                "{line}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_codex_answer_written_as_json_reaches_the_user_unchanged() {
        // `codex exec` prints the answer as plain text on stdout, so a JSON
        // line in it is the model's answer, never a protocol frame.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "cat"]);
        let mut chunks = Vec::new();
        let result = run_cli(
            &mut cmd,
            CliRun::plain("Codex", JSON_ANSWER.to_owned()),
            |text| {
                chunks.push(text);
                Ok(())
            },
            parse_codex_line,
        )
        .await;
        let joined = chunks.concat();
        println!("{result:?}\n{joined}");
        assert_eq!(result, Ok(()));
        assert_eq!(joined, JSON_ANSWER);
    }

    // ---------------- quitting and stopping ----------------

    /// Whether `pid` still runs. A killed process its parent has not reaped
    /// yet is a zombie (`Z`), which no longer runs.
    #[cfg(unix)]
    fn runs(pid: &str) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| !rest.starts_with('Z') && !rest.starts_with('X'))
        })
    }

    #[cfg(unix)]
    #[test]
    fn quitting_ends_the_cli_and_deletes_its_screenshot() {
        let dir = fresh_dir("quit");
        // One file a crash left behind, and the one the running request holds.
        std::fs::write(dir.join("aigc-shot-left-behind.png"), b"OLD").unwrap();
        let shot = TempFile::create(shot_path(&dir), b"PNGDATA").unwrap();
        let (pid_tx, pid_rx) = std::sync::mpsc::channel();
        let handle = tauri::async_runtime::spawn(async move {
            let _shot = shot;
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "echo $$; exec sleep 60"]);
            let ended = run_cli(
                &mut cmd,
                CliRun::plain("Quit", String::new()),
                |pid| pid_tx.send(pid).map_err(|e| e.to_string()),
                parse_codex_line,
            )
            .await;
            println!("the request ended: {ended:?}");
        });
        let ai = super::super::AiState::default();
        super::super::install_in(&ai.chat, 1, false, || handle).unwrap();
        let pid = pid_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the CLI started");
        let pid = pid.trim().to_owned();
        assert!(runs(&pid), "the CLI {pid} is not running");

        ai.stop_for_exit(Some(&dir));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while runs(&pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let still_runs = runs(&pid);
        if still_runs {
            std::process::Command::new("kill")
                .args(["-9", &pid])
                .status()
                .unwrap();
        }
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        std::fs::remove_dir_all(&dir).unwrap();
        println!("after quit: CLI running {still_runs}, files {left:?}");
        assert!(!still_runs, "the CLI kept running after quit");
        assert!(left.is_empty(), "files left after quit: {left:?}");
    }

    /// The processes a CLI starts on Windows, and how a test watches them.
    #[cfg(windows)]
    mod windows_tree {
        #![expect(
            unsafe_code,
            reason = "the test opens, waits on and ends a process by its pid"
        )]

        use super::*;
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
            WaitForSingleObject,
        };

        const START_A_CHILD: &str = "$i = New-Object System.Diagnostics.ProcessStartInfo \
            'ping.exe', '-n 60 127.0.0.1'; $i.UseShellExecute = $false; \
            $i.RedirectStandardOutput = $true; $p = [System.Diagnostics.Process]::Start($i); \
            [Console]::Out.WriteLine($p.Id); [Console]::Out.Flush(); Start-Sleep -Seconds 60";

        /// An npm-installed CLI is `node` plus the CLI's own binary, which
        /// `node` starts. Stop must end that second process too.
        #[tokio::test]
        async fn stopping_a_request_ends_the_process_its_cli_started() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let request = tokio::spawn(async move {
                let mut cmd = Command::new("powershell.exe");
                cmd.args(["-NoProfile", "-NonInteractive", "-Command", START_A_CHILD]);
                let ended = run_cli(
                    &mut cmd,
                    CliRun::plain("Tree", String::new()),
                    |line| tx.send(line).map_err(|e| e.to_string()),
                    parse_codex_line,
                )
                .await;
                println!("the request ended: {ended:?}");
            });
            let pid: u32 = loop {
                let line = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
                    .await
                    .expect("the CLI printed its child's pid")
                    .expect("the CLI is still running");
                if let Ok(pid) = line.trim().parse() {
                    break pid;
                }
            };
            // SAFETY: OpenProcess takes plain values; the handle is closed below.
            let started =
                unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) }
                    .unwrap();

            request.abort();
            println!("stopped: {:?}", request.await);

            // SAFETY: `started` is the open handle from above.
            let wait = unsafe { WaitForSingleObject(started, 5000) };
            if wait != WAIT_OBJECT_0 {
                // SAFETY: as above; ends what the test started when Stop did not.
                crate::util::log_if_err("end the test's process", unsafe {
                    TerminateProcess(started, 1)
                });
            }
            // SAFETY: `started` is not used again.
            crate::util::log_if_err("CloseHandle(process)", unsafe { CloseHandle(started) });
            assert_eq!(
                wait, WAIT_OBJECT_0,
                "process {pid}, started by the CLI, outlived the request"
            );
        }
    }
}
