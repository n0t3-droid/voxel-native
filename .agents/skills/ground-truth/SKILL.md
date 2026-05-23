---
name: ground-truth
description: 'Authoritative real-world reference skill. USE WHEN code, simulation, formula, or explanation involves real-world quantities: physics constants, SI units & conversions, material properties, gravity / drag / buoyancy / optics / acoustics, astronomical data, biological/anatomical sizes, vehicle/architectural dimensions, electrical & mechanical engineering values, color spaces (sRGB / linear / OKLab / CIE XYZ / HSL / HSV / Lab), color codes (HEX / RGB / CMYK / Pantone / CSS named), coding standards (RFCs, IEEE 754, Unicode, encoding), digital display calculations (DPI / PPI / aspect ratio / refresh / nits / HDR PQ / Rec.709 / Rec.2020 / DCI-P3), or any "exact value" question. EXTRACT exact numbers with units, cite the standard/source, and cross-check dimensional consistency BEFORE writing code or claims. DO NOT USE FOR: pure code refactors, style decisions, or opinion questions where no real-world ground truth exists.'
---

# Ground Truth Reference

When a task touches the physical world, color, or any standard, **stop guessing**. Look up exact values, attach units, verify dimensions, and cite the source — then write code or prose.

## When to Use

Trigger on any of:

- **Physics / mechanics**: gravity, friction, drag, buoyancy, momentum, rotational inertia, wave speed, optics, thermodynamics, fluid flow.
- **Units & conversions**: SI ↔ imperial, prefixes (n, µ, m, k, M, G, T), angle (rad/deg/turn), pressure, energy, power, luminous flux, dose.
- **Real-world sizes**: human anatomy, vehicle dimensions, architectural scale, terrain features, planetary radii, atomic/molecular scales.
- **Material properties**: density, modulus, yield, conductivity, refractive index, specific heat, hardness.
- **Astronomy**: orbital periods, distances, magnitudes, sun/moon angular size, day length, axial tilt, sidereal vs synodic.
- **Color**: gamma 2.2 vs sRGB EOTF, linear vs encoded RGB, OKLab/Lab/HSL/HSV/HCL, CIE XYZ/xyY, Rec.709/sRGB/DCI-P3/Rec.2020 primaries, white points (D65/D50/D55), Bradford / CAT02 chromatic adaptation, Pantone / CMYK approximation.
- **Color codes & digital display**: HEX `#RRGGBB[AA]`, `rgb()/rgba()/hsl()/oklch()`, alpha pre/un-multiplied, 8/10/12-bit, HDR (PQ/HLG), nits, Rec. ITU-R BT.* transfer functions, refresh, aspect ratios, DPI/PPI, sub-pixel layouts.
- **Coding & data standards**: IEEE 754 (binary32/64) ranges & rounding, two's complement, endian, UTF-8/16/32, Unicode normalization (NFC/NFD), RFC date/time/URI/JSON, ISO 8601, RFC 5322 email, RFC 4122 UUIDs, base64/base32/base58, RFC 1918 IP ranges, semver.
- **Electrical / signal**: V/A/Ω/W, dBV/dBu/dBFS, sample rates (44.1k, 48k, 96k), bit depth, Nyquist, common impedances.
- **Anything labeled "real-world", "realistic", "to scale", "physically accurate", "correct gamma", "true color"**.

Skip when the question is about taste, code style, or made-up worlds with no ground-truth claim.

## Procedure

### 1. Identify the Ground-Truth Items

List, before writing anything else, the **named quantities** the task depends on. Each entry must have:
- a symbol or short name,
- a **unit** (or "dimensionless"),
- the **standard / source** that defines it (NIST, CODATA, ISO, ITU-R, IEC, IEEE, CIE, RFC #, Unicode version, manufacturer datasheet, etc.).

If a quantity has *no* authoritative standard (e.g. "average human walking speed"), record the **range** and source of the central value, not a single magic number.

### 2. Look Up Exact Values

Use the [reference cheat-sheet](./references/cheatsheet.md) for the most common items. For anything not in the cheat-sheet:
- Prefer **CODATA 2022** for physical constants, **NIST** for units and material properties, **CIE** for color, **ITU-R BT.\*** for display, **IEEE 754-2019** for floats, **Unicode 15+** for text.
- Record the **value, unit, uncertainty (if known), and source**. No rounded-from-memory numbers.

### 3. Dimensional Sanity Check

Before any formula reaches code:
- Write the equation with units attached to every variable.
- Confirm both sides reduce to the **same SI base units**.
- Reject any line where units don't cancel — that catches ~90% of "off by a factor of 1000" bugs (μ vs m, kHz vs Hz, mm vs m, deg vs rad).

### 4. Choose the Right Color Pipeline

- Math (lighting, blur, tone-mapping, alpha blend) is done in **linear** RGB, not sRGB encoded.
- Convert sRGB → linear with the **piecewise sRGB EOTF**, not naive `pow(x, 2.2)` (the toe matters for shadows).
- Perceptual interpolation (gradients, palette generation) goes through **OKLab / OKLCH**, not HSL.
- Display output is encoded back: linear → sRGB OETF → 8-bit (or → PQ for HDR).
- Pantone, CMYK, named CSS colors are **approximations** — note the gamut and the white point.

### 5. Pick the Right Numeric Type

- Money / currency → integer minor units or fixed-point decimal, never `f32`/`f64`.
- Angles → radians internally; degrees only at the user boundary.
- Time → monotonic clock for durations; UTC + offset (RFC 3339) for instants.
- Geographic coords → `f64`; `f32` loses ~1 m precision and degrades fast at high lat.
- Counters that can grow → `u64` / `i64`, not `u32`.

### 6. Cite & Inline

In the code or answer, leave a short comment with:
- the value,
- the unit,
- the source / standard.

Example: `const EARTH_GRAVITY_M_PER_S2: f64 = 9.80665; // standard gravity, CGPM 1901`

### 7. Self-Audit Before Returning

Run this checklist:

- [ ] Every numeric literal has a unit (in name or comment).
- [ ] Every formula was unit-checked.
- [ ] Color math is in linear space; gamma applied exactly once.
- [ ] No `f32` for money, time-of-day, or coordinates that need it.
- [ ] No "approximately 9.8" / "roughly 3.14" — use the exact constant or `std::f64::consts`.
- [ ] Sources cited for any value a reviewer might question.

## Quality Bar

Output from this skill must be:

1. **Sourced** — every non-trivial number traceable to a standard.
2. **Unit-correct** — dimensional analysis passes.
3. **Pipeline-correct** — color/time/numeric type choices match the domain.
4. **Reproducible** — another engineer can re-derive the answer from the cited sources without contacting you.

## Anti-patterns

- Hand-typed constants from memory (`9.8`, `3.14`, `1.6e-19`).
- "Linear-ish" `pow(x, 2.2)` instead of the real sRGB EOTF.
- Mixing degrees and radians in the same expression.
- HSL for "perceptually even" gradients (it isn't).
- 8-bit color buffers for intermediate lighting math (banding).
- `f32` lat/lon, `f32` money, `f32` Unix time.
- Pantone / CMYK numbers presented as exact RGB without naming the gamut + white point.
- Treating "1 ft = 0.3 m" or "1 mile = 1.6 km" as exact.
- Citing Wikipedia *value* without checking the underlying standard it links.
