# Ground-Truth Cheat-Sheet

Frequently needed exact values. Sources in parentheses. Use these literally — do not retype from memory.

## Physical Constants (CODATA 2022, exact unless noted)

| Symbol | Name | Value | Unit |
|--------|------|-------|------|
| c | speed of light in vacuum | 299 792 458 | m/s (exact, SI def.) |
| h | Planck constant | 6.626 070 15 × 10⁻³⁴ | J·s (exact) |
| ħ | reduced Planck | 1.054 571 817 × 10⁻³⁴ | J·s |
| e | elementary charge | 1.602 176 634 × 10⁻¹⁹ | C (exact) |
| kB | Boltzmann constant | 1.380 649 × 10⁻²³ | J/K (exact) |
| NA | Avogadro number | 6.022 140 76 × 10²³ | mol⁻¹ (exact) |
| G | gravitational constant | 6.674 30 × 10⁻¹¹ | m³·kg⁻¹·s⁻² (uncertain ~2×10⁻¹⁵) |
| g₀ | standard gravity | 9.806 65 | m/s² (exact, CGPM 1901) |
| ε₀ | vacuum permittivity | 8.854 187 8188 × 10⁻¹² | F/m |
| µ₀ | vacuum permeability | 1.256 637 061 27 × 10⁻⁶ | N/A² |
| σ | Stefan–Boltzmann | 5.670 374 419 × 10⁻⁸ | W·m⁻²·K⁻⁴ |
| Patm | standard atmosphere | 101 325 | Pa (exact) |
| ρair | air density (15 °C, 1 atm) | 1.225 | kg/m³ |
| ρwater | water density (4 °C) | 999.972 | kg/m³ |

## Earth & Astronomy (IAU / NASA fact sheets)

| Quantity | Value | Unit |
|----------|-------|------|
| Earth equatorial radius (WGS84) | 6 378 137.0 | m (exact) |
| Earth polar radius (WGS84) | 6 356 752.314 245 | m |
| Earth flattening (WGS84) | 1 / 298.257 223 563 | — |
| Mean Earth radius | 6 371 000 | m |
| Earth mass | 5.9722 × 10²⁴ | kg |
| Sidereal day | 86 164.0905 | s |
| Solar day | 86 400 | s (mean) |
| Earth axial tilt | 23.4393 | deg |
| Astronomical unit (AU) | 149 597 870 700 | m (exact, IAU 2012) |
| Light-year | 9.460 730 472 580 8 × 10¹⁵ | m |
| Parsec | 3.085 677 581 491 4 × 10¹⁶ | m |
| Sun radius | 6.957 × 10⁸ | m (IAU nominal) |
| Sun mass | 1.988 47 × 10³⁰ | kg |
| Moon mean radius | 1 737 400 | m |
| Earth–Moon mean distance | 384 400 000 | m |
| Sun apparent angular diameter | 0.5334 | deg (mean) |
| Moon apparent angular diameter | 0.5181 | deg (mean) |

## Common Lengths (engineering / human scale)

| Item | Value | Unit | Source |
|------|-------|------|--------|
| Average adult human height (M / F, global) | 1.71 / 1.59 | m | WHO/NCD-RisC |
| Adult eye height standing | 1.62 | m | anthropometric mean |
| Walking speed (preferred) | 1.4 | m/s | Bohannon 1997 |
| Running, jog | 2.5–3.5 | m/s | — |
| Sprint, world-class | ~10 | m/s | IAAF |
| Stair step rise / run (residential) | 0.175 / 0.275 | m | IBC |
| Door (interior) | 0.81 × 2.03 | m | NA standard |
| Car length (sedan) | 4.6–4.9 | m | typical |
| Bus / coach | 12 | m | typical |
| Truck (semi w/ trailer) | 16.5 | m | EU directive |
| Football pitch (FIFA) | 105 × 68 | m | FIFA |
| Olympic pool | 50 × 25 × 2 | m | FINA |
| Empire State Building roof | 381 | m | as-built |
| Cruising airliner altitude | ~10 700 | m | FL350 |

## Unit Conversions (exact unless noted)

| From → To | Factor |
|-----------|--------|
| in → m | 0.0254 (exact) |
| ft → m | 0.3048 (exact) |
| yd → m | 0.9144 (exact) |
| mile → m | 1 609.344 (exact) |
| nautical mile → m | 1 852 (exact) |
| lb → kg | 0.453 592 37 (exact) |
| oz → g | 28.349 523 125 (exact) |
| US gal → L | 3.785 411 784 (exact) |
| UK gal → L | 4.546 09 (exact) |
| psi → Pa | 6 894.757 293 168 |
| bar → Pa | 100 000 (exact) |
| mmHg / Torr → Pa | 133.322 387 415 |
| cal (thermochem) → J | 4.184 (exact) |
| kWh → J | 3 600 000 (exact) |
| eV → J | 1.602 176 634 × 10⁻¹⁹ (exact) |
| °C → K | + 273.15 (exact) |
| °F → °C | (F − 32) × 5/9 (exact) |
| deg → rad | π / 180 |
| rev → rad | 2π |

## IEEE 754 Floating Point (IEEE 754-2019)

| Type | Bits (S/E/M) | Precision (decimal) | Min normal | Max | Notes |
|------|--------------|---------------------|------------|-----|-------|
| binary16 (half) | 1/5/10 | ~3.3 | 6.10 × 10⁻⁵ | 65 504 | gpu/ml |
| binary32 (float) | 1/8/23 | ~7.2 | 1.175 × 10⁻³⁸ | 3.403 × 10³⁸ | |
| binary64 (double) | 1/11/52 | ~15.95 | 2.225 × 10⁻³⁰⁸ | 1.798 × 10³⁰⁸ | |
| binary128 (quad) | 1/15/112 | ~34.0 | — | — | |

- ULP at 1.0 (double) = 2⁻⁵² ≈ 2.220 × 10⁻¹⁶.
- Default rounding: roundTiesToEven.
- `0.1 + 0.2 != 0.3` (canonical example).

## Color — sRGB (IEC 61966-2-1)

White point **D65** (x = 0.3127, y = 0.3290).

Primaries (CIE xy):

- R = (0.6400, 0.3300)
- G = (0.3000, 0.6000)
- B = (0.1500, 0.0600)

**sRGB EOTF (encoded → linear)**, channel-wise on [0,1]:

```text
linear = (s / 12.92)                           if s ≤ 0.040 45
       = ((s + 0.055) / 1.055) ^ 2.4           otherwise
```

**sRGB OETF (linear → encoded)**:

```text
s = 12.92 · linear                              if linear ≤ 0.003 130 8
  = 1.055 · linear^(1/2.4) − 0.055              otherwise
```

Naive `pow(x, 2.2)` is **wrong** for the toe; use the piecewise form.

## Color — Other Spaces

| Space | White | Notes |
|-------|-------|-------|
| sRGB / Rec.709 | D65 | gamut and primaries identical, transfer curves differ |
| Display-P3 | D65 | wider gamut, sRGB EOTF |
| DCI-P3 | DCI white (~6300 K) | cinema, gamma 2.6 |
| Rec.2020 | D65 | UHDTV, much wider gamut |
| Rec.2100 PQ (HDR10) | D65 | absolute, peak 10 000 nits |
| Rec.2100 HLG | D65 | relative, BBC/NHK |
| Adobe RGB (1998) | D65 | gamma 2.2 + small toe |
| ProPhoto | D50 | very wide, prepress |
| CIE XYZ | varies | linear, device-independent |
| CIE Lab | D50 (graphic arts) / D65 (display) | perceptual, older |
| OKLab / OKLCH | D65 | better hue uniformity than HSL/HCL — preferred for gradients & palettes |
| HSL / HSV | sRGB cylinder | NOT perceptually uniform |

CSS color names: 147 (CSS Color Module Level 4). `transparent` = `rgba(0,0,0,0)`.

## Display

| Standard | Resolution | Refresh | Bit depth | EOTF |
|----------|-----------|---------|-----------|------|
| 1080p | 1920×1080 | 24/30/60 | 8 | sRGB / BT.709 |
| 1440p | 2560×1440 | 60–240 | 8/10 | sRGB |
| 4K UHD | 3840×2160 | 24/30/60/120 | 10 | sRGB / PQ / HLG |
| 8K UHD | 7680×4320 | 60/120 | 10/12 | PQ / HLG |
| DCI 4K | 4096×2160 | 24 | 12 | gamma 2.6 |
| iPhone Retina | 326+ ppi | — | — | — |

- 1 nit = 1 cd/m². Typical office monitor 250–400 nits, HDR mastering 1000+ nits, PQ peak 10 000 nits.
- DPI vs PPI: DPI = print, PPI = display. Common confusion.

## Audio

| Item | Value |
|------|-------|
| Speed of sound (dry air, 20 °C) | 343.2 m/s |
| Sample rates (CD / pro / DAT) | 44 100 / 48 000 / 96 000 / 192 000 Hz |
| Bit depth (CD / pro) | 16 / 24 |
| Nyquist limit | f_s / 2 |
| dBFS reference | digital full scale |
| dBu reference | 0.7746 V RMS |
| Equal-loudness contour | ISO 226:2023 |

## Networking & Coding

| Item | Value / Standard |
|------|------------------|
| Private IPv4 ranges (RFC 1918) | 10/8, 172.16/12, 192.168/16 |
| Loopback | 127.0.0.0/8 (v4), ::1 (v6) |
| MTU (Ethernet) | 1500 bytes |
| TCP/UDP ports (well-known) | 0–1023 |
| HTTPS | TCP 443 |
| Date-time | RFC 3339 / ISO 8601 |
| Email address | RFC 5321 / 5322 |
| URI | RFC 3986 |
| UUID | RFC 4122 (v1, v4, v7) |
| JSON | RFC 8259 |
| UTF-8 / UTF-16 / UTF-32 | Unicode 15.1 (or current) |
| Base64 | RFC 4648 (with padding); base64url = URL-safe alphabet |

## Density / Strength (typical engineering values)

| Material | Density (kg/m³) | Young's modulus (GPa) | Yield strength (MPa) |
|----------|----------------|------------------------|----------------------|
| Air (15 °C) | 1.225 | — | — |
| Water (4 °C) | 999.972 | — | — |
| Steel (mild A36) | 7 850 | 200 | 250 |
| Aluminium 6061-T6 | 2 700 | 69 | 276 |
| Concrete | 2 400 | 17–30 | (compressive 30) |
| Glass (soda-lime) | 2 500 | 70 | (tensile ~50) |
| Pine wood | 350–500 | 9–13 | (parallel-grain ~40) |
| Titanium Gr5 | 4 430 | 114 | 880 |

Always cross-check with the actual datasheet for safety-critical work.

## Fluids — Drag & Buoyancy

- Drag force: F_d = ½ · ρ · v² · C_d · A
- Cd typical: sphere ~0.47, cube ~1.05, streamlined teardrop ~0.04, modern car 0.25–0.30, person standing 1.0–1.3.
- Buoyancy: F_b = ρ_fluid · V_displaced · g.
- Reynolds: Re = ρvL/µ. µ_air ≈ 1.81 × 10⁻⁵ Pa·s @ 15 °C; µ_water ≈ 1.002 × 10⁻³ Pa·s @ 20 °C.

## Optics

- Refractive index: vacuum 1.000 000, air 1.000 293, water 1.333, crown glass 1.52, diamond 2.417.
- Snell: n₁ sin θ₁ = n₂ sin θ₂.
- Visible light: ~380–780 nm.
- D65 correlated color temperature: 6504 K (CIE).

## Time

- 1 day (UT) = 86 400 SI s; leap seconds may be inserted (UTC).
- Julian year = 365.25 d (exact, astronomy).
- Tropical year ≈ 365.242 19 d.
- Unix epoch: 1970-01-01T00:00:00Z (no leap seconds in POSIX time).

---

When in doubt, **cite the standard, write the unit, run the dimensional check.**
