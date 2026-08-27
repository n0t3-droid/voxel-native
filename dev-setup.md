# Voxel Native development setup

This guide prepares a Windows development environment for the native Rust
engine and its optional WebAssembly build.

## 1. Install the Rust toolchain

Install Rust, the formatting and linting components, and optional maintenance
tools:

```powershell
winget install --id=Rustlang.Rustup -e --silent
rustup default stable
rustup component add rustfmt clippy
cargo install cargo-audit --version 0.22.2 --locked
cargo install cargo-watch --locked
```

## 2. Install Windows build tools

Install the current Visual Studio Build Tools with the **Desktop development
with C++** workload. Rust's Windows MSVC target uses its linker and Windows SDK.

## 3. Configure Visual Studio Code

Open the repository folder and install the extensions recommended by
[`.vscode/extensions.json`](.vscode/extensions.json). The checked-in workspace
configuration provides:

- a portable CodeLLDB launch configuration that resolves the Cargo binary;
- a release-build task;
- a release-run task whose output remains visible in a dedicated terminal; and
- an optional WebAssembly build task.

Use `Esc` to release the pointer whenever the native engine owns camera input.

## 4. Enable WebAssembly builds (optional)

```powershell
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.118 --locked
pwsh -NoProfile -File .\scripts\build-web.ps1
```

The build script writes generated bindings to `web/pkg/`, which is intentionally
ignored by Git.

## 5. Build, run, and verify

```powershell
cargo run
cargo build --release
cargo run --release
cargo watch -x "build --release"
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
cargo test --workspace --quiet
cargo check --target wasm32-unknown-unknown --bin voxel-native
cargo audit
.\scripts\elite-release-gates.ps1
```

Visual or streaming changes also require the deterministic native routes and
manual screenshot/report inspection defined in
[Responsive Visual QA](docs/RESPONSIVE_VISUAL_QA.md). Use a unique QA world for
each run; generated `qa_runs/`, saves, and local control files are never source
changes.
