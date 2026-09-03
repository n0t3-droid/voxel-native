Dev-Setup für Voxel-Native (Windows)

Kurzanleitung, um die Entwicklerumgebung einzurichten:

1) Rust toolchain

Installiere Rust und nützliche Komponenten. Voraussetzung: **Rust 1.77+**
(Bevy 0.14); aktuelles `stable` von rustup reicht. `blake3` ist auf 1.8.2
gepinnt — 1.8.3+ braucht edition2024 (Rust 1.85+) und bricht ältere
Toolchains schon beim Manifest-Parse ab (`feature edition2024 is required`).

```powershell
winget install --id=Rustlang.Rustup -e --silent
rustup default stable
rustup component add rustfmt clippy
cargo install cargo-audit --force
cargo install cargo-watch --force
```

Launch (from the repo root):

```powershell
.\run.ps1              # cargo run --release
.\run.ps1 -Qa          # release QA autopilot
cargo run --release
```

2) Windows Build Tools

Installiere die "Desktop development with C++" Workload (Visual Studio Build Tools) oder MSVC Toolchain.

3) VS Code

Installiere empfohlene Extensions (siehe `.vscode/extensions.json`) und nutze die Tasks/Launch Konfigurationen.

4) Web / WASM (optional)

```powershell
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --force
```

5) Nützliche Befehle

```powershell
cargo build --release
.\\target\\release\\voxel-native.exe
cargo watch -x "build --release"
cargo fmt --all
cargo clippy --all -- -D warnings
cargo audit
```

Wenn du willst, erstelle ich eine Branch, committe die Änderungen und richte einen PR ein (Push‑Berechtigung erforderlich).
