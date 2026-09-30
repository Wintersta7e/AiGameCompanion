# scripts

| Script | What it does | Run |
|---|---|---|
| `build.sh` | Release build of the launcher (vite build + `cargo xwin build`), copies `launcher.exe` + `config.example.toml` to `release/`. Also strips the build host's paths out of the binary -- from Rust via `--remap-path-prefix`, and from the C dependencies via `TARGET_CFLAGS_<target>`, which must repeat cargo-xwin's sysroot includes because aws-lc-sys replaces that variable instead of appending to it. | `./scripts/build.sh` |
| `ci-check.sh` | The step table and its runner: each check is one `row`; a plain run executes every row this machine can run and ends by naming each row it did not run and why; `--job <tag>` runs one CI job's rows; `--list` prints the table. Missing or different tool versions fail their rows. Run it after `cargo fmt` too. | `./scripts/ci-check.sh` |
