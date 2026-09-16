# scripts

| Script | What it does | Run |
|---|---|---|
| `build.sh` | Release build of the launcher (vite build + `cargo xwin build`), copies `launcher.exe` + `config.example.toml` to `release/`. Also strips the build host's paths out of the binary -- from Rust via `--remap-path-prefix`, and from the C dependencies via `TARGET_CFLAGS_<target>`, which must repeat cargo-xwin's sysroot includes because aws-lc-sys replaces that variable instead of appending to it. | `./scripts/build.sh` |
| `ci-check.sh` | Mirror the GitHub CI gate locally before pushing: `cargo fmt --check`, clippy (`--all-targets`), test, eslint, prettier `--check`, svelte-check (`--fail-on-warnings`), vite build, `npm audit`, cargo-deny, gitleaks. Run it after `cargo fmt` too. | `./scripts/ci-check.sh` |
