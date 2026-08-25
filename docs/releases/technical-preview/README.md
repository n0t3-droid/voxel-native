# Voxel Native Codex Engineering Atlas — technical preview

Download the [15-page engineering atlas](voxel-native-codex-engineering-atlas.pdf),
or inspect its [machine-readable provenance](voxel-native-codex-engineering-atlas.provenance.json).

This directory publishes one deliberately bounded artifact: a project-authored
technical atlas of Voxel Native's mathematics, architecture, budgets, evidence
identity, and rollback boundaries. It is not a runtime release verdict. The
runtime gallery remains visibly pending until separately manifest-backed native
captures complete the repository's visual-acceptance contract.

## Frozen artifact identity

- PDF SHA-256: `ff5c2ee81022e009f4bb2c025708a03a73856d7903e97e32ae62da12738aaa38`
- Size: `87,483` bytes
- Layout: `15` strict A4 pages, PDF 1.4, zero page rotation
- Document ID: `82DAAC126513C92915B023AF4CCEB451` in both trailer slots
- Aggregate 29-input fingerprint: `afe0fb87bda7d7a0062cf3df354388b2db462e2b1f7c5a6554199db233f3c0cd`
- Builder SHA-256: `8a107d21379a07250229daa176ef26c0634f360aed9809a5061c83b1a4b84c8c`

Codex produced two byte-identical deterministic builds, rendered all 15 final
pages to 1241 × 1754 PNGs at 150 DPI, and completed full-size visual review.
Independent review rechecked the formula-heavy and contrast-sensitive pages.
Structural validation found only Base-14 Type 1 fonts, no embedded font
programs, no scripts, forms, attachments, workstation paths, or replacement
glyphs, and 10 URI annotations resolving to nine allowlisted references.

The bundled Windows Poppler renderer emitted the same two font-discovery startup
diagnostics for both this atlas and a one-page Base-14 Helvetica control. Their
raw stderr streams were byte-identical, while the atlas's complete reachable
font graph contained neither named font. The provenance sidecar records the
comparison and hashes; the diagnostics are not hidden or reclassified as atlas
content.

## Reproduce and verify

Install the exact dependencies pinned in
[`tools/artifacts/requirements-atlas.txt`](../../../tools/artifacts/requirements-atlas.txt),
then run from the repository root:

```powershell
python -B tools/artifacts/test_build_codex_engineering_atlas.py
python -B tools/artifacts/build_codex_engineering_atlas.py --check-only
python -B tools/artifacts/build_codex_engineering_atlas.py --validate-release
python -B tools/publication/validate_repository_presentation.py
```

Any source, builder, toolchain, document-ID, link, font, active-content, page, or
binary drift makes canonical release validation fail closed.
