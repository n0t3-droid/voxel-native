# Contributing to Voxel Native

Voxel Native is active pre-release engine research. Contributions are welcome
when they preserve its central boundary: editable Near authority, bounded Far
representation, deterministic identity, and evidence-backed claims.

## Before starting

1. Read the current capability table in the [README](README.md) and the
   [Elite World Systems Standard](docs/ELITE_WORLD_SYSTEMS_STANDARD.md).
2. Open an issue before a broad architectural change, new retained-data format,
   or change that alters an authority, memory, work, or population ceiling.
3. For a vulnerability, follow [SECURITY.md](SECURITY.md) and do not disclose
   sensitive details in a public issue.

No project reuse license has been declared. Public visibility does not grant
permission to copy, modify, or redistribute the source or diagrams. If a
contribution depends on reuse terms or third-party material, stop and ask the
maintainers before submitting it. A pull request does not create a general
license for the repository.

## Development contract

- Use rustfmt defaults and preserve checked arithmetic, Euclidean signed-world
  mapping, deterministic ordering, explicit epochs, and fixed caps.
- Document a novel optimization's baseline, alternatives, budget, measured
  distribution, failure mode, and rollback boundary.
- Add adversarial tests for negative/extreme coordinates, stale asynchronous
  results, order independence, saturation, and exact byte/population ceilings
  when relevant.
- Keep research inputs distinct from implemented, live, and visually accepted
  capability.
- Never commit generated saves, QA runs, observer sessions, workstation paths,
  secrets, personal media, or local settings.

## Verification

Run the gates relevant to the change from the repository root:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
cargo test --workspace --quiet
cargo check --target wasm32-unknown-unknown --bin voxel-native
python -B tools/artifacts/test_build_codex_engineering_atlas.py
python -B tools/artifacts/build_codex_engineering_atlas.py --check-only
python -B tools/publication/test_validate_repository_presentation.py
```

Visual or streaming changes additionally require the documented viewport/DPI
matrix and matched native routes from one release executable. Inspect every
screenshot and its `report.ron`; average FPS or a completed PNG is not a pass.

## Pull requests

Keep the scope reviewable. State the authority boundary, fixed budgets, exact
commands run, native profiles/routes/viewports when applicable, known limits,
and rollback. Stage only reviewed paths. The pull-request template includes the
publication and user-data checks expected before merge.
