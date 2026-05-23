# voxel-native

Nativer Voxel-Engine-Nachfolger von **R93G** (<https://github.com/n0t3-droid/N5>).
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

## Autonomous QA / Screenshot Runs

Der native Build kann sich selbst testen: `--qa` startet eine Testwelt,
fliegt eine Kameraroute, speichert Screenshots und schreibt einen RON-Report
mit FPS, maximalem Frame-Time-Spike und Stalls.

```powershell
$env:VOXEL_NATIVE_QA='1'
$env:VOXEL_NATIVE_QA_SECONDS='45'
$env:VOXEL_NATIVE_QA_SCREENSHOT_INTERVAL='7'
.\target\release\voxel-native.exe --qa
```

Output landet unter `qa_runs\run_<timestamp>\`:

- `shot_0000.png`, `shot_0001.png`, ... — automatisch aufgenommene Bilder
- `report.ron` — Dauer, FPS, Chunk-/Mesh-Queues und alle Frame-Stalls >100ms

## Live Agent Control

`--agent-control` startet eine sichtbare Spielsession und liest laufend
`agent_control.ron`. Damit kann ein externer Agent ohne Benutzerklicks
Bewegung, Blickrichtung, Feuer, Scope, Screenshots und Exit steuern.
Der Modus hat im Fenster einen kleinen `AI LIVE`-Schalter: `AN` aktiviert die
Bridge, `AUS` gibt sofort alle Eingaben frei und laesst dich normal spielen.

```powershell
.\target\release\voxel-native.exe --agent-control
```

Beispiel für `agent_control.ron`:

```ron
(
   enabled: true,
   sequence: 1,
   forward: 1.0,
   right: 0.0,
   up: 0.0,
   sprint: true,
   fly: true,
   look_x: 0.35,
   look_y: -0.05,
   fire: false,
   scope: false,
   keys: [],
   mouse_buttons: [],
   game_state: "",
   build_mode: "",
   build_tool: "",
   handoff: false,
   screenshot: true,
   exit: false,
)
```

Eine kopierbare Vorlage liegt in `agent_control.example.ron`; beim Start wird
`agent_control.ron` automatisch erzeugt, falls sie fehlt.

`forward`, `right`, `up` sind Achsen von -1 bis 1. `look_x` und `look_y`
sind Maus-ähnliche Blickraten in Radiant pro Sekunde. Für einmalige Aktionen
wie Screenshot oder Exit `sequence` erhöhen. Status und Live-Screenshots
landen unter `agent_runs\live_<timestamp>\`.
`keys` und `mouse_buttons` fuettern die normalen Bevy-Input-Ressourcen. Damit
lassen sich echte Hotkeys und Klick-Tools testen, zum Beispiel:
`keys: ["F3"]` fuer Build Studio, `keys: ["Digit3"]` fuer Sniper,
`mouse_buttons: ["Left"]` fuer LMB oder `keys: ["ControlLeft", "P"]` fuer
den Command Deck Shortcut.
Fuer deterministische Tool-Matrix-Tests kann der Agent zusaetzlich
`build_mode: "live"` oder `build_mode: "picker"` und ein `build_tool` wie
`"DrawRect"`, `"Sculpt"`, `"SmartTower"`, `"BrushPlace"`, `"BrushCut"`,
`"CityRoad"`, `"CityDistrict"`, `"CityBuilding"`, `"CityFacade"` oder
`"AnimationPick"` setzen.

Um den Agenten im laufenden Spiel auszuschalten und normal weiterzuspielen,
klicke `AI LIVE: AUS` oder schreibe:

```ron
(
   enabled: false,
   sequence: 999,
   game_state: "ingame",
   build_mode: "combat",
   handoff: true,
)
```

Das laesst die Session offen, gibt synthetische Keys/Maus los, schliesst
Build-/Picker-Modi und blendet das Agent-Overlay aus. Inventar, Pause, F3,
Command Deck und normale Maus-/Keyboard-Eingaben bleiben danach wieder beim
Spieler. Ohne `--agent-control` startet der Build komplett ohne Agent-Bridge.

Der Agent-Test-Loop ist:

1. `cargo build --release --color never`
2. `.\target\release\voxel-native.exe --agent-control`
3. `agent_control.ron` mit neuer `sequence` schreiben
4. `agent_runs\live_<timestamp>\status.ron` lesen
5. `last_screenshot` mit Bildanalyse prüfen
6. bei Bugs neue Sequenz schreiben oder `exit: true` setzen

`status.ron` enthaelt unter anderem `game_state`, `command_sequence`,
aktive Waffe, aktives Tool, Position, Yaw/Pitch, FPS, Durchschnitts-FPS,
`max_frame_ms`, `stall_count`, Chunk-/Mesh-Queues, `last_error`, Screenshot-Zahl
und den letzten Bildpfad.
Damit kann ein Agent nicht nur spielen, sondern auch Performance-Spikes,
Parsing-Fehler, leere Screenshots und visuelle Regressionen protokollieren.
Im sichtbaren Fenster rendert der Modus zusaetzlich OCR-freundliche `OCR_*`
Zeilen mit State, Sequenz, Position, Frame-Zeiten, Fire/Scope und Fehlerstatus,
damit Vision-Modelle den Zustand direkt aus dem Bild lesen koennen.

Periodische Screenshots sind standardmäßig aus. Für automatische Capture-Läufe:

```powershell
$env:VOXEL_NATIVE_AGENT_SCREENSHOT_INTERVAL='3'
.\target\release\voxel-native.exe --agent-control
```

Erster Build dauert lange (Bevy zieht viele Crates). Folgende Builds
sind dank Incremental Compilation + `dynamic_linking`-Feature deutlich
schneller.

## Modul-Layout (gespiegelt zu R93G)

| Rust-Modul       | R93G-Pendant                            |
| ---------------- | --------------------------------------- |
| `src/blocks.rs`  | `lib/voxel/blocks.ts`                   |
| `src/chunk.rs`   | `lib/voxel/world.ts` (Chunk-Teil)       |
| `src/terrain.rs` | `lib/voxel/terrain.ts`                  |
| `src/mesher.rs`  | `lib/voxel/mesher.ts`                   |
| `src/world.rs`   | `lib/voxel/ChunkManager.ts`             |
| `src/player.rs`  | `components/Player.tsx` + `physics.ts`  |

## Roadmap (Reihenfolge: echte Wins zuerst)

1. **Chunk-Streaming** rund um den Spieler + Unload (aus `ChunkManager.ts`).
2. **Greedy Meshing** für mid/far LOD — drastisch weniger Dreiecke.
3. **Echter Terrain-Stack**: Domain Warping + Ridged FBM + Narrow-Band-Caves
   und Biomes (Temperature/Moisture) — 1:1 aus `terrain.ts` portiert.
4. **Block-Kollision + Gravitation** (aus `physics.ts`).
5. **Texture-Atlas** statt Vertex-Farben.
6. **Day/Night + Weather Shader** (aus `VoxelEngine.tsx` / `WeatherEditor.tsx`).
7. **Save/Load** (RON-Format, Serde ist schon drin).
