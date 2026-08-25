# World Look Continuum v1

Status: **mixed-state canonical contract, source snapshot 2026-08-26**.
Implemented surfaces remain native-acceptance pending; the unified
`WorldLookProfileV1`, whole-pass runtime baseline gate, and live whole-pass
rollback path are future promotion requirements.

![World Look Continuum v1 causal contract](media/world-look-continuum.svg)

Implementation truth is labelled rather than inferred:

| Capability | Source state in this snapshot | Acceptance state |
| --- | --- | --- |
| Scalar optical families | implemented in the built-in material library | native visual acceptance pending |
| Near water | four fixed integer-lattice modes and CPU-integrated modulo phases in one always-registered opaque `WaterSurfaceMaterial`; its eight-`Vec4` extension uniform is `128 B`, and it adds exactly one material asset | native visual acceptance pending |
| Far water | exact live copies of Near `wave_0`, `wave_1`, and the full `temporal_phase` in one stable six-`Vec4`, `96 B` uniform and one stable opaque material; existing Far Hydro meshes/entities are reused | native visual acceptance pending |
| Foliage response | four species presets, per-species CPU-integrated modulo phases in a three-`Vec4`, `48 B` uniform per existing material, and analytic normal correction are implemented | native visual acceptance pending |
| Atmosphere and camera grade | linear-light Natural/Astral sky, light, fog, saturation, and bounded camera grades are implemented through the existing `WorldProfile` | native visual acceptance pending |
| Unified `WorldLookProfileV1`, whole-pass runtime gate, and same-binary baseline fallback | not implemented | future promotion and rollback requirement |

“Implemented” in this table means source is present, not that a build, native
route, performance result, or visual verdict has passed.

## Decision

World Look Continuum v1 turns the existing Natural and Astral art directions
into one bounded, render-only causal system:

```text
world/profile identity + CPU frame delta + weather
                    |
                    v
             WorldLookProfileV1
          /       /       \       \
   materials   water   vegetation  atmosphere
          \       \       /       /
                    v
             near/far horizon
                    |
                    v
          matched visual evidence
```

The look profile may alter material scalars, analytic surface normals,
vegetation presentation, fog/sky colour, and render-only far aggregation. It
may not alter voxel values, terrain height, water occupancy, collision,
player/shuttle/projectile forces, bot navigation, edit records, saves, or city
authority. A beautiful frame is not permission to cross that boundary.

The complete V1 contract deliberately chooses:

1. scalar optical families for the 44 built-in material swatches;
2. the implemented four-mode, analytically differentiated,
   deep-water-inspired normal field for Near water and its implemented exact
   two-mode Far projection;
3. the existing two-band foliage motion with analytic normal correction;
4. the implemented Natural/Astral atmosphere and camera grades, followed by a
   future unified look-profile resource for every visual consumer;
5. fixed incremental shader, ALU, asset, entity, and rollback budgets; and
6. a future fail-closed runtime promotion path, followed by matched native
   visual evidence.

This is not a fluid simulation, an erosion model, a spectral renderer, a new
world generator, or a claim of measured performance.

## 1. Reproducible baseline

The materialized built-in palette contains block IDs `1..=44`; `Air` is not a
swatch. The baseline below records the pre-pass comparison state; it is not a
currently selectable runtime mode. At that baseline:

- `Water` has its own scalar profile;
- `Lava` has its own scalar profile;
- `Leaves`, `JungleLeaves`, `BlossomLeaves`, and `SakuraPetals` share the
  foliage branch, with two authored roughness values; and
- the remaining **38 of 44 built-ins collapse to one flat PBR profile**:
  perceptual roughness `1.0`, Bevy reflectance control `0.05`, metallic `0.0`.

The 38 collapsed materials are:

`Stone`, `Dirt`, `Grass`, `Sand`, `Wood`, `Snow`, `Ice`, `TundraGrass`,
`SavannaGrass`, `Gravel`, `Bedrock`, `RedSand`, `RedStone`, `MesaClay`,
`MossStone`, `Limestone`, `Crystal`, `Basalt`, `AlienMoss`, `BoneRock`,
`GlowSand`, `ShipHullDark`, `ShipHullAlloy`, `CockpitGlass`, `NeonCyan`,
`NeonMagenta`, `NeonAmber`, `EngineCore`, `LuminiteCrystal`, `MagnetiteOre`,
`IridiumVein`, `ZenStone`, `Bamboo`, `ShojiPaper`, `RoofTile`, `TatamiMat`,
`NeonGlass`, and `ShojiLamp`.

The flat scalar profile hides distinctions already present in the generated
albedo swatches. Ice and soil respond identically to light; hull alloy is not a
conductor; polished zen stone is as rough as dirt. This is the measured-in-code
baseline count, not a visual-quality verdict.

Other baseline facts constrain the candidate:

- all built-in albedo images are deterministic generated swatches rather than
  tracked raster textures;
- four foliage materials use the embedded vegetation extension;
- Near water uses a static baked ripple/caustic albedo;
- Far terrain uses one untextured material and canonical vertex colours;
- Far Hydro v1 deliberately forces water/lava alpha to `1` under global
  `Msaa::Off`; and
- semantic Far cohorts remain a separate, default-off visual gate.

The baseline is defined by
[`src/blocks.rs`](../src/blocks.rs),
[`src/textures.rs`](../src/textures.rs),
[`src/mesher.rs`](../src/mesher.rs),
[`src/vegetation.rs`](../src/vegetation.rs),
[`src/planetary_streaming.rs`](../src/planetary_streaming.rs), and
[`FAR_HYDROGRAPHIC_CONTINUITY_V1.md`](FAR_HYDROGRAPHIC_CONTINUITY_V1.md).
Uncommitted experiments and screenshots outside the repository are not part of
this baseline.

## 2. Ground-truth quantities, units, and claim boundary

| Symbol | Meaning | Contract value | Unit | Authority |
| --- | --- | ---: | --- | --- |
| `g_0` | standard acceleration of gravity | `9.80665` | `m s^-2` | exact conventional value; NIST/BIPM |
| `n_air` | incident-medium index used by V1 | `1.000` | dimensionless | authored air approximation |
| `n_water` | water index used by V1 | `1.333` | dimensionless | authored nominal visible-light approximation |
| `P` | exact world-phase period | `4096` | voxel metres | engine authoring constant |
| `Y_sRGB` | linear-sRGB relative-luminance weights | `(0.2126, 0.7152, 0.0722)` | dimensionless | W3C CSS Color 4 / WCAG 2.2, derived from IEC 61966-2-1; mirrored in [`src/daynight.rs`](../src/daynight.rs) |
| `Delta t_n` | frame delta consumed by CPU phase integration | finite and positive; otherwise ignored | `s` | engine clock |
| `x` | horizontal render position | finite | voxel metres | render-only coordinate |

One voxel world unit is mapped to one metre **only for the wave phase and
dispersion calibration in this presentation model**. Terrain height, river
grammar, rainfall, and runtime behavior remain authored voxel-world quantities; V1
does not turn them into measured hydrology.

`n_water = 1.333` is not presented as universal. Real refractive index and
underwater attenuation vary with wavelength, temperature, salinity, suspended
material, and dissolved constituents. V1 uses one nominal RGB-interface value
and explicitly defers participating-media transport.

Material/PBR input derivation and atmosphere palette, light, sky, and fog
mixing occur in linear RGB. Stored sRGB atmosphere colours are decoded exactly
once by the existing Bevy colour path. Camera section grading follows Bevy's
pipeline in linear HDR before ACES, while `post_saturation` is deliberately a
post-tonemap control; the contract does not mislabel that stage as linear-light
mixing. No `pow(rgb, 2.2)` shortcut is accepted.

## 3. Canonical target `WorldLookProfileV1`

No `WorldLookProfileV1` resource or runtime mode gate exists in the current
source snapshot. The Near-water plugin and material are registered
unconditionally. Far water currently copies Near water point-to-point, while
atmosphere and camera grading read the existing effective `WorldProfile`.
Those implemented links are not yet one versioned identity or one baseline
switch. The conceptual profile below is therefore a future promotion
requirement: it is a small, finite value object whose implementation may use a
Rust resource, shader uniform, or both, but has one identity and no collections
that grow with travel.

Required fields are:

- `mode`: `baseline` or `continuum-v1`;
- `world_profile`: `Natural` or `AstralFrontier`;
- canonical linear palette controls for terrain, water, lava, foliage,
  atmosphere, and restrained accents;
- weather strength and normalized horizontal wind direction;
- sun-elevation or existing day/night blend terms;
- four finite water-mode records, in stable order;
- exposure/bloom multipliers already bounded by the runtime profile; and
- an explicit version used by any cache or async result whose interpretation
  changes with the look profile.

After that wiring exists, every consumer must read the same normalized profile:

| Consumer | May consume | Must not consume or mutate |
| --- | --- | --- |
| Material optics | roughness, Bevy reflectance control, metallic, linear tint | voxel/material identity, save aliases |
| Water/lava | phase modes, interface F0, presentation tint, roughness | occupancy, water level, fluid category |
| Vegetation | existing weather response, species mode, presentation palette | forces, collision, pathfinding |
| Atmosphere | existing sky/fog/day-night blend and profile tint | terrain generation, celestial authority |
| Far horizon | canonical family/palette ID, filtered water-mode subset | Near edits, collision, save state |
| UI/ambient | optional profile accent hint | persisted user-selected UI theme |

Future gate/profile parsing must resolve unknown, non-finite,
version-mismatched, or unsupported values to the baseline profile. This is not
a claim about the current always-on implementation, which has no in-binary
baseline fallback.

## 4. Selected optical families

Bevy 0.14's `StandardMaterial.reflectance` is a remapped control `r`, not F0
itself. For a dielectric, the pinned shader computes

```text
F0_bevy = 0.16 r^2.
```

The following table is the selected finite candidate table. Values are
authoring constants, not material-laboratory measurements. `R` is perceptual
roughness, `r` is Bevy's reflectance control, and `M` is metallic.

| Optical family | Built-ins | `R` | `r` | `M` | Intended reading |
| --- | --- | ---: | ---: | ---: | --- |
| Loose soil | `Dirt` | `0.96` | `0.36` | `0.00` | broad, low-energy highlight |
| Living ground | `Grass`, `TundraGrass`, `SavannaGrass`, `AlienMoss` | `0.90` | `0.38` | `0.00` | diffuse but not chalk-flat |
| Granular ground | `Sand`, `RedSand`, `GlowSand` | `0.92` | `0.40` | `0.00` | dry granular response |
| Rough rock | `Gravel`, `Bedrock`, `Basalt` | `0.88` | `0.46` | `0.00` | muted mineral highlight |
| Cut/weathered rock | `Stone`, `RedStone`, `MesaClay`, `MossStone`, `Limestone`, `BoneRock` | `0.78` | `0.48` | `0.00` | shape-readable rock mass |
| Polished stone | `ZenStone` | `0.58` | `0.50` | `0.00` | calm, broad polished response |
| Fibrous | `Wood`, `Bamboo`, `TatamiMat` | `0.76` | `0.42` | `0.00` | soft elongated-looking highlight without tangent claim |
| Porous paper | `ShojiPaper` | `0.93` | `0.36` | `0.00` | diffuse warm panel |
| Ceramic tile | `RoofTile` | `0.64` | `0.50` | `0.00` | readable glazed/ceramic edge |
| Snow | `Snow` | `0.82` | `0.42` | `0.00` | bright diffuse surface without white clipping |
| Ice | `Ice` | `0.20` | `0.46` | `0.00` | tight dielectric highlight |
| Crystal/glass | `Crystal`, `CockpitGlass`, `LuminiteCrystal`, `NeonGlass` | `0.12` | `0.50` | `0.00` | tight dielectric volume cue |
| Neon dielectric | `NeonCyan`, `NeonMagenta`, `NeonAmber` | `0.22` | `0.48` | `0.00` | emissive colour with retained mid-tones |
| Dark manufactured hull | `ShipHullDark` | `0.42` | `0.52` | `0.72` | coated/conductive sci-fi plate |
| Bright alloy | `ShipHullAlloy` | `0.28` | `0.56` | `0.90` | strongest built-in conductor |
| Magnetite-bearing ore | `MagnetiteOre` | `0.54` | `0.50` | `0.58` | rough partial conductor |
| Iridium-bearing vein | `IridiumVein` | `0.30` | `0.54` | `0.78` | bright partial conductor |
| Engine core casing | `EngineCore` | `0.34` | `0.50` | `0.62` | conductive casing plus bounded emission |
| Lamp ceramic | `ShojiLamp` | `0.70` | `0.46` | `0.00` | warm rough body plus bounded emission |
| Active Near-water extension | `Water` | `0.16` base; top surface bounded to `0.11..=0.305` | `0.357` | `0.00` | one shared opaque custom material; nominal air/water interface |
| Lava | `Lava` | `1.00` | `0.05` | `0.00` | heat/emission, not polished plastic |
| Broad foliage | `Leaves`, `JungleLeaves` | `0.82` | `0.42` | `0.00` | waxy but restrained crown volume |
| Blossom/petal mass | `BlossomLeaves`, `SakuraPetals` | `0.74` | `0.42` | `0.00` | lighter foliage response |

The ordering is contractual: dirt must remain rougher than cut stone; cut
stone rougher than ice/crystal; glass and all ordinary ground remain
non-metallic; bright hull alloy remains the strongest conductor. Exact numbers
may change only through a new contract version or an evidence-backed correction
that updates this table and its tests together.

The ordinary built-in `StandardMaterial` record for `Water` still carries
roughness `0.18` and `AlphaMode::AlphaToCoverage`, but current Near-water bucket
selection supersedes that record with the custom extension. It is therefore not
a live fallback path.

Current alpha and emission truth is deliberately split:

- foliage stays `AlphaMode::Mask(0.42)` and double-sided;
- active Near water uses `AlphaMode::Opaque`, outputs alpha `1`, and remains
  depth-stable under the process-wide `Msaa::Off` policy;
- Lava and Far Hydro stay opaque under `Msaa::Off`;
- no built-in enters a new sorted transparent pass;
- Lava does not receive a second material-emission term on top of the existing
  vertex emission budget;
- unresolved custom material IDs retain the loud magenta failure material on
  ordinary Standard routes;
- Near Water is one deliberate exception: its authoritative render class
  suppresses every custom or unresolved base texture, including the magenta
  sentinel, and binds the one canonical `WaterSurfaceMaterial` instead; and
- Vegetation is the other bounded exception: the authoritative voxel species
  selects one of the four canonical foliage materials. A nonmatching built-in,
  custom, or unresolved base texture is suppressed rather than changing the
  species wind preset or allocating a material-by-species cross-product.

Shader-family authority is not inferred from editable `MaterialId`.
`MeshBucketKey` carries `MeshRenderClass` beside the material ID, and the voxel
category selects `Standard`, `Vegetation`, or `Water`. Consequently, a solid
voxel painted with Water's material ID remains on the Standard route, while a
Water voxel carrying any custom material ID remains on the Water-optics route.
For Water, the custom ID remains deterministic edit and mesh-bucket identity
only; suppressing its base texture is the explicit trade for the fixed single-
Water-material asset budget. The Vegetation class additionally carries one of
four bounded voxel-derived species discriminators. Leaves and Sakura sharing a
custom material ID therefore remain separate buckets and retain their own
canonical presets; the custom ID remains edit/bucket identity, but its base
texture is intentionally suppressed to keep the existing four-material budget.

## 5. Water phase, dispersion, normals, and Fresnel

### 5.1 Periodic wave vectors

V1 uses at most four directional modes. To keep phase stable at signed world
coordinate extremes, each mode uses an integer lattice vector
`q_i in Z^2 \ {0}` over the exact phase period `P = 4096 m`:

```text
kappa_i = (2 pi / P) q_i          [rad m^-1]
k_i     = ||kappa_i||             [rad m^-1]
lambda_i = 2 pi / k_i             [m]
d_i     = kappa_i / k_i           [dimensionless].
```

The implemented records derive wavelength from the integer vector; wavelength
is not an independent phase input:

| Mode | Fixed `q_i` | Exact `||q_i||` | Derived `lambda_i = P / ||q_i||` |
| --- | ---: | ---: | ---: |
| `0` | `(240, 128)` | `272` | approximately `15.0588235 m` |
| `1` | `(384, -288)` | `480` | approximately `8.5333333 m` |
| `2` | `(-576, 768)` | `960` | approximately `4.2666667 m` |
| `3` | `(560, -1920)` | `2000` | exactly `2.048 m` |

The shader constructs `kappa_i = (2 pi / 4096) q_i` directly. It does not
reconstruct phase from a normalized direction and a separately authored
wavelength. The four `q_i` never rotate, swap, or switch with weather. Smoothed
wind direction only redistributes a bounded `18%` amplitude-weighting share
toward aligned fixed modes, while smoothed wind strength scales the global
amplitude. Keeping `q_i` fixed prevents a weather transition from producing a
phase pop.

World coordinates are reduced with Euclidean integer modulo before conversion
to `f32`. Because both components of `q_i` are integers, adding `P` on either
horizontal axis adds an integer multiple of `2 pi` to the phase. The wrap is
therefore observationally continuous rather than merely numerically close.

The Near mesher's `texture_world_scale` gives the Water voxel category priority
over editable material identity: it returns exactly `0.125` repeats per voxel
for both built-in and custom Water IDs. The Near shader's exact `8.0`
UV-to-metre factor therefore reconstructs one metre per voxel in either case;
a custom ID cannot accelerate the spectrum or break the Far phase bridge.

### 5.2 Deep-water dispersion

For each mode:

```text
omega_i = sqrt(g_0 k_i)                                      [s^-1]
alpha_i^(n+1) = rem_euclid(alpha_i^n - omega_i Delta t_n, 2 pi)
theta_i^n = dot(kappa_i, x) + alpha_i^n + phi_i.
```

Dimensional check:

```text
[g_0 k_i] = (m s^-2)(m^-1) = s^-2
[omega_i] = s^-1
[kappa_i x] = 1
[omega_i Delta t_n] = 1.
```

Thus `theta_i` is dimensionless. `g_0 = 9.80665 m s^-2` is the exact standard
acceleration of gravity, not a locally measured planetary field. Rust computes
each `omega_i` from the fixed lattice norm, `P`, and `g_0`, then integrates the
four `alpha_i` values in `f64`. `advance_temporal_phase` consumes the complete
positive finite frame delta, including a hitch; it does not apply the separate
`0.100 s` weather-response clamp. Non-positive or non-finite deltas leave the
phase unchanged, and every accepted update is reduced to `[0, 2 pi)`.

Neither water shader imports or reads Bevy's renderer-global time. Near uploads
the four bounded CPU phases in one `Vec4`; Far receives an exact copy of that
entire vector and uses its `x/y` components for the retained modes.

The relation `omega = sqrt(gk)` is the deep-water, gravity-wave relation used
to coordinate phase speeds. V1 does not solve depth-dependent shallow-water
dispersion. Shore attenuation, if used, is an authored presentation mask and
must never be described as bathymetric simulation.

### 5.3 Analytic normal field

V1 evaluates a virtual height solely to derive a shading normal:

```text
h(x,n)        = sum_i A_i sin(theta_i^n)                  [m]
grad h(x,n)   = sum_i A_i kappa_i cos(theta_i^n)          [dimensionless]
n_water       = normalize(vec3(-d h/dx, 1, -d h/dz)).
```

The water mesh, water table, occupancy, collider, and voxel authority do not
move. The authored safety cap is

```text
sum_i |A_i| k_i <= 0.46,
```

so the virtual slope remains finite and the unnormalized normal retains a
positive vertical component. Current `q_i` are fixed constants and current
amplitudes are constructed from bounded CPU inputs; the shader does not accept
arbitrary profile records or expose a general runtime fallback switch. Once
runtime profile records exist, all `A_i`, `k_i`, `omega_i`, phases, and
intermediate results must be finite, and invalid input must resolve to a flat
normal.

Near evaluates all four fixed modes. Its extension uniform is exactly eight
`Vec4` values, **128 bytes**: four wave records, the four-component
`temporal_phase`, one optics record, and two linear-colour records. On top
faces, the fragment shader contains exactly two `normalize` invocations: one
for the analytic wave normal and one for the bounded blend with the geometric
normal.

Vertical Near-water faces take a mutually exclusive, sample-free closed-form
side-cue path instead of evaluating the spectral normal. For
`s = dot(uv, (0.73, 1.11))`, it forms

```text
r = 1 - |2 fract(0.19 s + alpha_0 / (2 pi)) - 1|     in [0,1]
c = r^2 (3 - 2r)                                     in [0,1]
side mix share = 0.18 + 0.32 c                        in [0.18,0.50].
```

This cue shares mode zero's CPU phase and adds no trigonometric call, texture
sample, or normalisation. It changes bounded colour only; it does not displace
the vertical voxel face.

Far receives exact live copies of `wave_0`, `wave_1`, the full four-component
`temporal_phase`, optics, and linear colour records through
`FarFieldFluidOpticsUniform::from_near`. Its uniform is exactly six `Vec4`
values, **96 bytes**, and belongs to one stable process-wide opaque Far fluid
material. The Far shader deterministically uses `temporal_phase.x/y` and
ignores `z/w`. A `Last` system copies changed Near parameters after weather and
phase response without a second time accumulator, weather state, queue, or
per-ring material.

The Far shader uses the same copied CPU phases and the same phase offsets
`0.31` and `1.73`; it constructs `kappa` directly from the copied integer `q`.
Far therefore neither reseeds, rotates, nor retimes the two longest modes.

Far phase UVs wrap only the integer ring anchor with Euclidean modulo `4096`.
The local `gx * step` and `gz * step` offsets remain unwrapped, so a triangle
crossing the period boundary interpolates one local step instead of backwards
through the period. The existing UV attribute stores `(phase_z, phase_x)` at
`0.125` repeats per metre to match Near's swizzle. Lava adds the disjoint
`8192` U marker and is selected at threshold `4096`; fluid kind is never
inferred from RGB. This implements deterministic low-frequency phase
continuity, but native visual acceptance of the Near/Far handoff and the
removal of modes `2` and `3` remains pending.

### 5.4 Fresnel interface

At normal incidence:

```text
F0 = ((n_air - n_water) / (n_air + n_water))^2
   = ((1.000 - 1.333) / (1.000 + 1.333))^2
   = 0.020373187841971414
   = 2.0373187841971414%.
```

For Bevy 0.14's dielectric mapping:

```text
r_water = sqrt(F0 / 0.16)
        = 0.35683669095585074,
```

authored as `0.357`. Fresnel variation over view angle remains the pinned PBR
shader's Schlick approximation; V1 must not multiply a second Fresnel term over
it.

The Fresnel value is an interface calibration, not proof of correct reflected
scene content. V1 adds no screen-space reflection, refraction buffer,
environment capture, underwater spectral volume, or caustic render target.

## 6. Vegetation modes and analytic normal correction

V1 retains the four existing render-only foliage modes:

| Foliage material | Macro amplitude `[voxel]` | Macro angular rate `[rad s^-1]` | Flutter amplitude `[voxel]` | Flutter angular rate `[rad s^-1]` |
| --- | ---: | ---: | ---: | ---: |
| `Leaves` | `0.13` | `0.85` | `0.035` | `5.7` |
| `JungleLeaves` | `0.10` | `0.70` | `0.028` | `5.1` |
| `BlossomLeaves` | `0.16` | `0.95` | `0.050` | `6.4` |
| `SakuraPetals` | `0.19` | `1.05` | `0.060` | `7.2` |

Each mode combines:

- a slow directional macro sine;
- a slower gust-envelope sine;
- two incommensurate faster flutter sines;
- height/position local variation; and
- a cross-wind share.

Each existing foliage material carries three `Vec4` values, **48 bytes**:
`direction_macro`, `flutter_phase`, and `temporal_phase`. The CPU owns four
`f64` phases for each of the four species. For smoothed weather strength `w`,
the per-species temporal rates are

```text
M_s = authored_macro_rate_s   * (0.72 + 0.28 w)
F_s = authored_flutter_rate_s * (0.78 + 0.22 w)
Omega_s = (0.37 M_s, M_s, F_s, 1.71 F_s)
psi_s^(n+1) = rem_euclid(psi_s^n + Omega_s Delta t_n, 2 pi).
```

As with water, vegetation phase integration consumes the complete positive
finite frame delta and ignores non-positive or non-finite deltas. Distinct
authored species rates therefore remain distinct without reading
`globals.time`, and weather-frequency changes do not reset phase. The shader's
fixed work is exactly five `sin` and five `cos` calls: the cosine terms are the
analytic derivatives of the same five phase families used for displacement.

Weather changes normalized direction plus bounded amplitude and temporal-rate
response. CPU-integrated phases preserve continuity across those rate changes.
Calm air retains the existing small micro-flutter floor. The total authored
displacement remains below the existing conservative `0.35 voxel` culling
expansion.

### 6.1 Deformation Jacobian

Let the render-only horizontal offset be `u = (u_x, u_z)` and

```text
f(x,y,z) = (x + u_x(x,y,z), y, z + u_z(x,y,z)).
```

Its Jacobian is

```text
    [ 1 + u_x,x   u_x,y   u_x,z     ]
J = [ 0           1       0         ]
    [ u_z,x       u_z,y   1 + u_z,z ].
```

Normals transform by the inverse transpose. Division by the determinant is
unnecessary before normalization, so the shader uses the cofactor matrix:

```text
n' = normalize(cofactor(J) n).
```

Writing

```text
a = 1 + u_x,x   b = u_x,y   c = u_x,z
d = u_z,x       e = u_z,y   f = 1 + u_z,z
Delta = af - cd,
```

the unnormalized corrected normal is

```text
cofactor(J)n = (
    f n_x - d n_z,
    (ce - bf)n_x + Delta n_y + (bd - ae)n_z,
    -c n_x + a n_z
).
```

The derivatives are analytic derivatives of the same phases that create the
offset. Finite-difference re-evaluation, stale undeformed normals, and a
separate normal-noise phase are rejected.

The source-level proof for every current foliage preset and weather strength
must retain these strict bounds:

| Bound | Required result |
| --- | ---: |
| `|d u / d spatial_phase|` | `< 0.50` |
| `|d u / d y|` | `< 0.064` |
| horizontal Jacobian deviation from identity | `< 0.12` |
| horizontal determinant floor | `> 0.88` |

If the cofactor-transformed normal's squared length is not greater than
`1e-12`, or is greater than `1e20`, the shader falls back to the normalized
undeformed world normal. The first comparison fails closed for `NaN`; the
upper guard catches infinity and extreme finite values. Forward, prepass, and
deferred paths call the same correction.

V1 does not extend wind to trunks, grass, Bamboo, or Far cohorts. Those require
base-anchor semantics and separate silhouette QA; moving them without that
contract would trade one visible discontinuity for floating roots and sliding
ground.

## 7. Atmosphere and Near/Far continuity

### 7.1 Implemented atmosphere and camera grades

The current atmosphere reads the existing effective `WorldProfile`. Authored
sRGB palette triples are decoded exactly once with Bevy's piecewise sRGB
transfer, then daylight, twilight, and night are interpolated in linear RGB.
The resulting linear sky drives clear colour and the fog/horizon tint; it is
not converted back to encoded sRGB for intermediate lighting math.

Biome saturation is luminance-relative in linear sRGB:

```text
Y  = dot(rgb, (0.2126, 0.7152, 0.0722))
s  = clamp(finite_saturation, 0.72, 1.48); non-finite -> 1
rgb' = clamp(Y + (rgb - Y) s, 0, 1).
```

The affine step preserves `Y` before the final gamut clamp; the clamp keeps
every output finite and bounded but may alter luminance at a gamut boundary.

Natural preserves the established sky endpoints and lighting palette. Astral
adds a darker indigo/cobalt foundation blended at `0.52`, a warmer directional
key, a cooler fill, and ambient brightness scale `0.96`. The player camera then
applies these bounded profile grades:

| Profile | Exposure | Post-saturation | Shadows `(contrast, lift)` | Midtones `(saturation, contrast, gain)` | Highlights `(saturation, contrast, gain)` |
| --- | ---: | ---: | ---: | ---: | ---: |
| Natural | `-0.06 EV` | `1.015` | `(1.015, 0.003)` | `(1.01, 1.055, 0.985)` | `(0.97, 0.975, 0.92)` |
| Astral Frontier | `-0.04 EV` | `1.03` | `(1.02, 0.006)` | `(1.0, 1.05, 0.98)` | `(0.94, 0.96, 0.86)` |

Both grades leave hue, temperature, and tint at zero. These source paths are
implemented; native visual acceptance across time, biome, weather, viewport,
and graphics-profile routes remains pending.

### 7.2 Shared invariants

Continuity means shared cause and controlled loss, not identical geometry.

- Near and Far derive canonical linear albedo from the same `BlockType` family.
- Far may aggregate material families, but it may not introduce a second hand-
  copied palette.
- Water/lava category remains independent of colour; no shader infers fluid
  authority by comparing RGB values.
- Near shader routing follows `MeshRenderClass`, not editable `MaterialId`: a
  solid borrowing Water's ID stays Standard, while Water with any custom ID
  uses the one canonical optics material and retains that ID only as edit and
  bucket identity.
- Near Water keeps exactly `0.125` UV repeats per voxel even under a custom ID,
  so the shader's `8.0` reconstruction and Far's metre-phase convention agree.
- Far water uses byte-identical live copies of the two longest-wavelength Near
  records and the complete CPU-integrated phase vector, with the same fixed
  phase offsets and no renderer-global time read.
- Far anchor wrapping and unwrapped local UV offsets preserve exact periodic
  phase without adding geometry or classification-by-colour.
- **Future unified-profile wiring:** profile version, seed, and whole-pass mode
  must participate in stale-result identity wherever async publication depends
  on them.
- Far deterministically omits modes `2` and `3`; no new random sample replaces
  them, and the perceptual handoff remains a native-QA obligation.
- Fog and exposure hide neither a missing ring nor a category error.

### 7.3 Distance bands

| Band | Required information | Permitted loss |
| --- | --- | --- |
| Near | generated swatch, optical family, four water modes, foliage pores and corrected normals | none of the authoritative block/material ID |
| Play | family response and crown volume; streamed Near water keeps four modes while Far Hydro uses its exact two-mode projection | subpixel pores and the two omitted Far water frequencies |
| Horizon | canonical linear palette, macro silhouette, implemented two-mode Far water, atmosphere separation | microtexture, individual leaves, local edits outside Near authority |

The Far terrain entity and hydro entity budgets do not change. A future
semantic-cohort promotion remains governed by
[`FAR_SEMANTIC_COHORTS_V1.md`](FAR_SEMANTIC_COHORTS_V1.md); this contract does
not silently enable it.

## 8. Natural and Astral art targets

### Natural

- Meadow, soil, bark, limestone, snow, and mineral water remain the dominant
  hierarchy; neon does not leak into ordinary ecology.
- Water reads blue-green and reflective at grazing angles without becoming an
  electric cyan plane at noon.
- River V3's submerged bed, sediment shelf, and living cap remain legible from
  walking and flight cameras.
- Fog follows the existing clear-sky colour and separates terrain masses
  rather than washing them into one grey layer.
- Foliage remains muted enough that trunks, light wells, and terrain openings
  survive motion.

### Astral Frontier

- Indigo/cobalt atmosphere and rose nebula remain a backdrop, not a full-frame
  magenta grade.
- Cyan/violet crystal, dark basalt/hull, and restrained amber/magenta accents
  form a readable value hierarchy.
- Water and lava remain distinct from crystals and neon signage at night.
- Bloom enriches emissive landmarks but cannot erase landing surfaces,
  silhouettes, or cockpit guidance.
- The same world rules must remain convincing away from the authored hero
  precinct.

### Shared rejection conditions

Both profiles fail if any route shows plastic soil, metallic vegetation,
mirror-like Far terrain, featureless slab-like Near-water walls, transparent
Far depth-order artifacts, synchronised forest sliding, undeformed lighting
normals, frozen wind under active weather, hue clipping, or a Near/Far phase
jump. `Opaque` is the required Near-water render/depth mode in this snapshot,
not the desired perceptual reading; bounded colour, roughness, Fresnel, and
normal variation must still make the surface read as water.

## 9. Fixed budgets

These are design ceilings, not measured timings or compiled GPU instruction
counts. "Scalar ALU" means explicit source-level add/subtract/multiply/FMA-
equivalent work in the custom extension, excluding Bevy's existing PBR shader.
Trigonometric and reciprocal-square-root operations are listed separately
because compiler lowering is hardware-dependent.

### 9.1 Shader and bounded glue-work ceilings

| Path | Modes | Scalar ALU ceiling | Trig ceiling | Reciprocal square root | Dynamic loops | Added texture samples |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Scalar optical families | `0` | `0` custom | `0` | `0` | `0` | `0` |
| Near water top-surface optics | `<= 4` | `<= 96` | exactly `8` (`4 sin`, `4 cos`) | exactly `2` normalize paths | `0` | `0` |
| Far water top-surface optics | `<= 2` | `<= 64` | exactly `4` (`2 sin`, `2 cos`) | `<= 1` | `0` | `0` |
| Vegetation displacement + analytic normal | fixed two-band mode | `<= 128` | exactly `10` (`5 sin`, `5 cos`) | `<= 2` | `0` | `0` |
| Atmosphere/profile tint glue | CPU constant-work | `<= 32` new CPU scalar glue; `0` custom shader | `0` | `0` | `0` | `0` |

An implementation that exceeds a row must reduce work or create v2. A driver
assembly listing may refine the performance discussion, but it cannot weaken
these source-level ceilings or be reported as cross-GPU truth.

The mutually exclusive Near vertical-face cue uses zero trigonometric calls,
zero normalize calls, and zero texture samples. Vegetation's reciprocal-square-
root ceiling covers direction normalization plus either the guarded corrected-
normal inverse square root or its normalized geometric fallback.

### 9.2 Asset, memory, entity, and job ceilings

| Increment over baseline | Hard ceiling |
| --- | ---: |
| New embedded WGSL shader assets | `2` |
| New steady-state material assets | `4` |
| Temporary material assets during replace-in-place rebuild | `8` |
| New decoded/generated image assets | `0` |
| New mesh assets | `0` |
| New samplers, render targets, storage textures, or storage buffers | `0` |
| New profile/extension uniform payload | `512 B` total, excluding allocator metadata |
| New ECS entities | `0` |
| New draw calls for an unchanged visible material bucket set | `0` |
| New async jobs or queues | `0` |
| New colliders, navigation records, or save records | `0` |

The current water implementation consumes both new shader slots and owns two
stable custom material instances. Near adds one embedded `water_optics.wgsl`,
one process-wide `WaterSurfaceMaterial`, and a `128 B` extension uniform. Far
adds the embedded `far_water_optics.wgsl`; its one fixed
`FarFieldFluidMaterial` and `96 B` extension uniform replace the former
one-per-process Far Hydro `StandardMaterial` on enabled routes rather than
adding a per-ring material population. Far optics reuses the existing Far
Hydro UV attribute, meshes, fluid-ring entities, draw buckets, and jobs, so it
adds **zero** images, textures, samplers, meshes, entities, draw calls, or async
jobs. Atmosphere and profile grading add no shader, image, mesh, material,
entity, draw call, job, collider, navigation record, or save state.

Near material IDs still participate in deterministic bucket identity, but the
authoritative Water render class always reuses that single Near material. A
custom Water material therefore cannot add a texture-bound Water material or
an unresolved-sentinel variant; this is why the single-material count remains
true even when edited Water carries arbitrary custom IDs.

Vegetation applies the same bounded policy at species granularity. The editable
material ID remains in `MeshBucketKey`, but the voxel-derived species selects
one of the four existing foliage materials. Custom or borrowed base textures do
not allocate extension variants, so the species-material population stays
exactly four rather than growing toward the material-by-species cross-product.

Each of the four existing vegetation materials now carries `48 B`, for
`192 B` total foliage-extension payload. Near water (`128 B`) plus Far water
(`96 B`) plus foliage (`192 B`) is **`416 B` of current custom-extension
payload**, excluding allocator alignment and `StandardMaterial`. The recorded
baseline already carried two `Vec4` values (`32 B`) on each foliage material,
so the actual candidate increment is **`288 B`**: `128 + 96 + 4 * 16`. Both
figures stay inside the `512 B` contract ceiling; they are layout accounting,
not a GPU-memory-allocation measurement.

The existing fully enabled Far ceiling remains at most six terrain entities,
six fluid entities, and one optional L5 semantic-cohort entity: **13 total**.
The existing fully enabled atomic Far worker payload ceiling remains
`757,984 B`; implemented Far optics changes neither Far mesh topology nor
worker payload.

The four existing vegetation material assets remain the complete wind-update
work set. Each update performs at most four existing-uniform writes. No
per-tree, per-leaf, per-wave, per-cell, or per-frame material asset may be
created.

### 9.3 Failure and pressure behavior

Current source truth is narrower than the desired fallback contract:

- Near-water lattice records are compile-time finite constants, and CPU-side
  weather strength/direction is normalized and bounded before upload.
- Water phase integration ignores non-positive/non-finite deltas, consumes the
  complete positive finite delta in `f64`, and reduces every phase modulo
  `2 pi`; its weather smoothing alone uses the bounded response delta.
- The Near-water plugin, shader, resource, and material are always registered.
  A missing or rejected custom shader does **not** currently select the
  ordinary `StandardMaterial` water path through a runtime mode switch.
- Far starts from a finite flat 96-byte fallback if Near parameters are not
  available at initialization, then retains its last finite state until the
  `Last` synchronizer can copy Near. This is a Far-material fallback, not a
  whole-pass baseline mode.
- Vegetation applies the same full-delta/modulo rule independently to four
  phases for each fixed species. Its shader rejects degenerate, `NaN`,
  infinite, and extreme corrected normals through the bounded fallback above.
- Atmosphere converts authored sRGB exactly once, maps non-finite saturation to
  `1`, clamps saturation to `[0.72, 1.48]`, and clamps the final linear output
  to `[0,1]`.
- No current low-spec policy drops the two shortest modes or all custom water
  work.

Before promotion, the future gate must add and test these fail-closed rules:

- non-finite profile or shader input selects baseline scalar values, a flat
  water normal, and a normalized undeformed foliage normal;
- missing custom shader/material support selects the existing
  `StandardMaterial` path;
- low-spec pressure first drops the two shortest water modes, then all custom
  water-normal work, without shortening the Far horizon or changing fluid
  kind;
- reduced motion may suppress ornamental amplitude but must preserve
  selection, material identity, and atmosphere readability; and
- a material-library reload replaces assets in place within the caps rather
  than creating an unbounded trail of handles.

## 10. Future rollout and rollback contract

There is **no reversible World Look Continuum runtime gate in the current
implementation**. `WaterOpticsPlugin` is registered unconditionally,
`WaterSurfaceLibrary` creates its one Near material during app initialization,
Planetary Streaming initializes and synchronizes its one Far material, and the
atmosphere/camera grade follows the effective `WorldProfile`. The existing
`VOXEL_NATIVE_FAR_HYDROGRAPHY=off` switch can disable the Far Hydro subsystem;
it is not a baseline switch for Near water, materials, vegetation, atmosphere,
or the whole visual pass. The current whole-pass rollback boundary is therefore
a source/build change, not a live runtime switch.

The following name is reserved as a future promotion requirement; it must not
be advertised as an available current interface:

```text
VOXEL_NATIVE_WORLD_LOOK_CONTINUUM=continuum-v1
```

Once implemented, explicit aliases may include `v1`, `on`, `1`, and `true`.
Missing, unknown, malformed, `baseline`, `off`, `0`, `false`, `disabled`, and
`none` must resolve fail-closed to the baseline until native acceptance
promotes v1.

That future mode must be non-persistent and must not rewrite user settings or
saves. It must participate in material/Far presentation identity and
stale-result rejection where interpretation differs. Switching modes may
rebuild existing bounded materials and render-only Far assets, but it may not
regenerate voxel authority, delete caches broadly, or migrate a world.

Future same-binary rollback is complete only when:

1. the gate resolves to `baseline`;
2. the original StandardMaterial and vegetation paths are resident;
3. no v1 async result can publish under baseline identity;
4. incremental v1 materials are released through ordinary asset lifetime;
5. entity, worker-payload, save, and collision counts match baseline; and
6. a paired baseline route completes without shader/log errors.

No rollback step may delete `saves/`, `qa_runs/`, `agent_runs/`, custom
materials, or user media.

## 11. Candidate comparison

| Candidate | Benefit | Cost/risk | Decision |
| --- | --- | --- | --- |
| Scalar family split + analytic water/foliage normals + shared look profile | fixes the largest material and motion discontinuities without new images/entities | bounded custom shader work; requires native temporal QA | **Selected for v1** |
| One texture set per material with normal/roughness/metal maps | high local detail | tracked/decoded asset growth, authoring burden, repetition and paging problem | Rejected for v1 |
| FFT ocean or full Gerstner displacement | rich large-water geometry | mismatched rivers/pools, tessellation and authority questions, higher work | Deferred behind isolated water prototype |
| Screen-space reflection/refraction | richer water reflections | depth-order holes, off-screen loss, global `Msaa::Off` and render-target cost | Rejected for v1 |
| Navier-Stokes, flood fill, or shallow-water solver | dynamic physical-looking flow | new simulation authority, queues/state, save/collision ambiguity | Rejected |
| Spectral participating-media water | wavelength-dependent underwater appearance | outside surface-continuum scope and budget | Research boundary only |
| Finite-difference foliage normals | simpler derivation | extra displacement evaluations, epsilon/scale sensitivity, phase drift | Rejected |
| Leave original normals after deformation | zero added normal work | moving shape and static lighting disagree visibly | Rejected |
| Per-block custom shader | maximum local control | pipeline/material explosion and difficult rollback | Rejected |
| Global colour grade only | cheap apparent unity | hides rather than repairs material/scale causes; clips profile distinction | Rejected |

NVIDIA's water chapter motivates bounded analytic modes and direct derivative
normals; it does not make this implementation a rigorous water simulation.
Eurographics 2024 underwater spectral work establishes that real water-body
appearance varies spectrally and with participating media; V1 cites that as a
boundary and does not borrow its reported performance or claim equivalent
transport.

## 12. Verification and visual-QA gates

### 12.1 Static and pure gates

Before a graphical run, tests must establish:

- exactly 44 built-in swatches and complete family assignment;
- at least ten distinct optical signatures and all scalar values finite in
  `[0,1]`;
- the semantic ordering from Section 4;
- water `F0` agrees with the stated IOR equation within `1e-4` after Bevy's
  reflectance remap;
- integer-period phase is invariant under `+/- P` on both axes and remains
  finite at `i32::MIN` and `i32::MAX` world coordinates;
- render-class routing is independent of editable material identity: a solid
  with Water's ID remains Standard, Water with a custom ID remains Water, the
  bucket keys remain distinct, and only the latter suppresses its custom or
  unresolved base texture in favour of the canonical Water material;
- authoritative foliage species is likewise independent of material identity:
  a solid with Leaves' ID remains Standard, custom Leaves remains the Leaves
  preset, Leaves painted as Sakura does not borrow Sakura motion, and Leaves
  and Sakura sharing one custom ID remain distinct deterministic buckets while
  reusing the fixed four canonical foliage materials;
- `texture_world_scale` remains exactly `0.125` for Water under both built-in
  and custom material IDs, preserving the Near `8.0` UV-to-metre inverse and
  the Near/Far phase bridge;
- Near's uniform remains eight `Vec4` values (`128 B`); Far's `wave_0`,
  `wave_1`, and full `temporal_phase` are byte-identical live copies of Near,
  its uniform remains six `Vec4` values (`96 B`), and Far uses phase components
  `x/y` with the same fixed offsets;
- water angular frequencies are CPU-derived from `g_0`, `P`, and each lattice
  norm; one `0.75 s` phase update agrees with three `0.25 s` updates within the
  source test's `1e-12` `f64` tolerance, both water shaders contain no
  `globals.time`, the Near fragment retains exactly two `normalize` calls, and
  its vertical side cue remains bounded and sample-free;
- wrapped-anchor/unwrapped-local Far UVs remain periodic and finite at extreme
  anchors, and the disjoint lava marker never enters colour authority;
- the current four fixed water records are finite, nonzero, period-compatible,
  and slope-capped; after profile modes become runtime data, invalid records
  fail to a flat normal;
- all four vegetation presets remain inside displacement and derivative
  bounds, and every per-material uniform remains three `Vec4` values (`48 B`);
- per-species vegetation phases remain finite and modulo-bounded under full
  finite hitches, the shader contains no `globals.time` and exactly five
  `sin`/five `cos` calls, and forward, prepass, and deferred paths use the same
  corrected normal with the finite/extreme fallback;
- atmosphere endpoints are decoded once and interpolated in linear RGB,
  luminance-relative saturation is finite and bounded, both world profiles
  remain distinct, and both camera grades stay inside their safety envelope;
- after profile/gate wiring exists, mode/cache identity rejects stale results;
- asset/entity/job/worker-payload ceilings are exact; and
- after runtime fallback exists, WebAssembly compilation either supports the
  selected path or visibly and honestly falls back to baseline.

### 12.2 Matched native routes

Final visual acceptance requires the same release binary, fixed seed, camera
route, viewport, profile, and settings, first with `baseline`, then with
`continuum-v1`. The current implementation has no whole-pass switch and cannot
perform that same-binary pair; it may collect candidate screenshots, but cannot
satisfy this gate or be promoted until the runtime switch exists. At minimum:

1. Natural river-bank route: bed, shelf, living cap, water interface, trees.
2. Natural broad-biome route: soil/rock/snow/ice ordering and Far handoff.
3. Astral hero route: crystal, basalt, neon, lava, landing surfaces, nebula.
4. Astral non-hero route: proves the grammar is not a single staged shot.
5. Near/Far transition flight: water phase, material family and horizon.
6. Calm-to-storm stationary route: foliage offset and corrected lighting.
7. Noon, sunset, night, fog, rain, and snow states where supported.

Every motion route captures at least two separated stationary frames and a
short sequence. Inspect for shimmer, phase reset, chunk-wide sliding,
undeformed highlights, frozen motion, z-fighting, sorting artifacts, bloom
clipping, and exposure pumping.

### 12.3 Viewport, profile, and evidence matrix

Use the full matrix in
[`RESPONSIVE_VISUAL_QA.md`](RESPONSIVE_VISUAL_QA.md), including the primary
`1920 x 1080` frame, minimum `960 x 540`, legacy `800 x 600`, `1280 x 720`,
`2560 x 1440`, `3440 x 1440`, narrow/portrait diagnostics, and affected
100%/150%/200% OS scale cases. Exercise Fast, Balanced, and High graphics
profiles.

The report must bind:

- source and executable identity;
- exact continuum mode and world profile once the gate/profile exists; until
  then, the report must state `always-on candidate; no runtime baseline`;
- seed, route, camera positions, phase names, and capture timestamps;
- physical viewport for every terminal PNG;
- current/peak asset, entity, queue, and worker-payload counters;
- shader compilation/log state;
- frame-time distribution after separately reported warm-up; and
- a manual disposition for every screenshot and anomaly.

The performance targets remain those in
[`ELITE_WORLD_SYSTEMS_STANDARD.md`](ELITE_WORLD_SYSTEMS_STANDARD.md). This
contract records no new frame-time, GPU-time, power, or satisfaction result.
Average FPS alone cannot accept the pass.

### 12.4 Promotion rule

V1 may become the accepted default only after Far-water and atmosphere native
evidence passes, the future whole-pass runtime rollback gate exists, and all
required Natural and Astral routes:

- pass their static and native gates with one release binary;
- remain within every fixed budget;
- show no unresolved authority or transition defect;
- include manually inspected screenshots and matching reports; and
- preserve a working `baseline` rollback route.

If any required matrix cell is absent, the verdict is **not accepted**, not
"probably correct".

## 13. Internal and primary sources

Current implementation snapshot:

- [`src/main.rs`](../src/main.rs) for unconditional plugin registration and
  process-wide `Msaa::Off`;
- [`src/water.rs`](../src/water.rs) for the one-material library, fixed lattice
  records, CPU-derived deep-water rates, full-delta modulo phase integration,
  weather amplitude weighting, optical bounds, and source tests;
- [`src/mesher.rs`](../src/mesher.rs) for authoritative `MeshRenderClass`,
  material-preserving bucket identity, the bounded foliage-species
  discriminator, and Water-first `0.125` UV scale;
- [`assets/shaders/water_optics.wgsl`](../assets/shaders/water_optics.wgsl) for
  direct `kappa = (2 pi / 4096) q` phase construction, CPU phase consumption,
  two-normalize top optics, and the sample-free vertical side cue;
- [`src/planetary_streaming.rs`](../src/planetary_streaming.rs) plus
  [`assets/shaders/far_water_optics.wgsl`](../assets/shaders/far_water_optics.wgsl)
  for the exact `96 B` Far projection, full phase-vector copy, stable material,
  wrapped-anchor/unwrapped-local UVs, and disjoint lava marker;
- [`src/world.rs`](../src/world.rs) for category-authoritative shader routing,
  canonical Water/foliage material selection, and intentional suppression of
  custom Water and Vegetation base textures and the unresolved sentinel;
- [`src/textures.rs`](../src/textures.rs) for optical families and the inactive
  ordinary water `StandardMaterial` record;
- [`src/vegetation.rs`](../src/vegetation.rs) plus
  [`assets/shaders/vegetation_wind.wgsl`](../assets/shaders/vegetation_wind.wgsl)
  for four CPU phase accumulators per species, `48 B` uniforms, fixed
  five-sine/five-cosine work, analytic derivatives, and guarded corrected
  normals;
  and
- [`src/daynight.rs`](../src/daynight.rs) plus
  [`src/player.rs`](../src/player.rs) for linear-light sky/light/fog
  interpolation, luminance-relative saturation, Natural/Astral palettes, and
  bounded camera grades.

Internal repository contracts:

- [`CODEX_ENGINEERING_ATLAS.md`](CODEX_ENGINEERING_ATLAS.md)
- [`ELITE_WORLD_SYSTEMS_STANDARD.md`](ELITE_WORLD_SYSTEMS_STANDARD.md)
- [`RESPONSIVE_VISUAL_QA.md`](RESPONSIVE_VISUAL_QA.md)
- [`NATURAL_RIVER_BANK_V3.md`](NATURAL_RIVER_BANK_V3.md)
- [`FAR_HYDROGRAPHIC_CONTINUITY_V1.md`](FAR_HYDROGRAPHIC_CONTINUITY_V1.md)
- [`FAR_SEMANTIC_COHORTS_V1.md`](FAR_SEMANTIC_COHORTS_V1.md)
- [`RENDERING_RESEARCH_NOTES.md`](RENDERING_RESEARCH_NOTES.md)
- [`VOXEL_DISCOVERY_ATLAS.md`](VOXEL_DISCOVERY_ATLAS.md)

Direct primary/official links:

- NIST, standard acceleration of gravity `g_n = 9.80665 m s^-2`:
  <https://physics.nist.gov/cgi-bin/cuu/Value?gn=>
- Mark Finch, NVIDIA GPU Gems, *Effective Water Simulation from Physical
  Models*:
  <https://developer.nvidia.com/gpugems/gpugems/part-i-natural-effects/chapter-1-effective-water-simulation-physical-models>
- Zioma, NVIDIA GPU Gems 3, *GPU-Generated Procedural Wind Animations for
  Trees*:
  <https://developer.nvidia.com/gpugems/gpugems3/part-i-geometry/chapter-6-gpu-generated-procedural-wind-animations-trees>
- Sousa, NVIDIA GPU Gems 3, *Vegetation Procedural Animation and Shading in
  Crysis*:
  <https://developer.nvidia.com/gpugems/gpugems3/part-iii-rendering/chapter-16-vegetation-procedural-animation-and-shading-crysis>
- Monzon, Gutierrez, Akkaynak, and Munoz, Eurographics 2024 / Computer
  Graphics Forum, *Real-Time Underwater Spectral Rendering*:
  <https://diglib.eg.org/items/1316f247-e9a8-48fe-8754-f3276191e6b5>

These sources provide transfer principles and ground-truth boundaries. They do
not establish Voxel Native's performance, correctness, or visual acceptance;
only this repository's tests and matched native evidence can do that.
