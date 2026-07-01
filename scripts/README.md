# scripts

| Script | What it does | Run |
|---|---|---|
| `build.sh` | Release build of the launcher (vite build + `cargo xwin build`), copies `launcher.exe` + `config.example.toml` to `release/`. | `./scripts/build.sh` |
| `ci-check.sh` | Mirror the GitHub CI gate locally before pushing: `cargo fmt --check`, clippy, test, prettier `--check`, svelte-check, vite build, gitleaks. Run it after `cargo fmt` too. | `./scripts/ci-check.sh` |
