# voxel-native

Nativer Voxel-Engine-Nachfolger von **R93G** (https://github.com/n0t3-droid/N5).
Gebaut mit **Rust + Bevy** (wgpu → Vulkan / DX12 / Metal). Kein Browser,
kein Electron — echte native Performance.

## Status

Scaffold / Tag 0. Läuft bereits:

- 3D-Fenster mit Himmel + Sonnenlicht
- Chunk-Datenstruktur (16×16×16)
- Einfacher Terrain-Generator (Perlin-Heightmap, Grass/Dirt/Stone)
- Face-culled Mesher (nur sichtbare Flächen)
- Fly-Kamera (WASD + Maus, Space/Shift hoch/runter, Ctrl = schneller, Esc = Maus frei)

## Build & Run

```powershell
# Debug (schneller kompilieren, spielbar dank opt-level=1 + deps opt=3)
cargo run

# Release (volle Performance)
cargo run --release
```

Erster Build dauert lange (Bevy zieht viele Crates). Folgende Builds
sind dank Incremental Compilation + `dynamic_linking`-Feature deutlich
schneller.

## Modul-Layout (gespiegelt zu R93G)

| Rust-Modul      | R93G-Pendant                       |
| --------------- | ---------------------------------- |
| `src/blocks.rs` | `lib/voxel/blocks.ts`              |
| `src/chunk.rs`  | `lib/voxel/world.ts` (Chunk-Teil)  |
| `src/terrain.rs`| `lib/voxel/terrain.ts`             |
| `src/mesher.rs` | `lib/voxel/mesher.ts`              |
| `src/world.rs`  | `lib/voxel/ChunkManager.ts`        |
| `src/player.rs` | `components/Player.tsx` + `physics.ts` |

## Roadmap (Reihenfolge: echte Wins zuerst)

1. **Chunk-Streaming** rund um den Spieler + Unload (aus `ChunkManager.ts`).
2. **Greedy Meshing** für mid/far LOD — drastisch weniger Dreiecke.
3. **Echter Terrain-Stack**: Domain Warping + Ridged FBM + Narrow-Band-Caves
   + Biomes (Temperature/Moisture) — 1:1 aus `terrain.ts` portiert.
4. **Block-Kollision + Gravitation** (aus `physics.ts`).
5. **Texture-Atlas** statt Vertex-Farben.
6. **Day/Night + Weather Shader** (aus `VoxelEngine.tsx` / `WeatherEditor.tsx`).
7. **Save/Load** (RON-Format, Serde ist schon drin).
```
