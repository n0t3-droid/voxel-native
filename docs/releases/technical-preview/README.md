# Voxel Native Codex Engineering Atlas — technical preview

Download the [15-page engineering atlas](voxel-native-codex-engineering-atlas.pdf),
or inspect its [machine-readable provenance](voxel-native-codex-engineering-atlas.provenance.json).

This directory publishes one deliberately bounded artifact: a project-authored
technical atlas of Voxel Native's mathematics, architecture, budgets, evidence
identity, and rollback boundaries. It is not a runtime release verdict. The
runtime gallery remains visibly pending until separately manifest-backed native
captures complete the repository's visual-acceptance contract.

## Frozen artifact identity

- PDF SHA-256: `bfff95e72999456d280b0637001356b7bbe9e44dc891770308f1a561cf4bea3b`
- Size: `87,475` bytes
- Layout: `15` strict A4 pages, PDF 1.4, zero page rotation
- Document ID: `95519471C8CA18F54FAEEA1E1557C5CF` in both trailer slots
- Aggregate 30-input fingerprint: `61403b99156294794e433245e3156d4894f87bc1535999bea250b1cf240343ea`
- Builder SHA-256: `c357f17281f96aa65aa758c8a46549f9b16272fc87e3360f04cee892ee733349`

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
