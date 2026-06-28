# Rendering Research Notes

These notes translate the user's rendering references into voxel-native work.
They are not implementation-complete and should not be treated as current
engine behavior.

## References

- Nick McDonald's high-performance voxel engine article:
  `https://nickmcd.me/2021/04/04/high-performance-voxel-engine/`
- LearnOpenGL instancing chapter:
  `https://learnopengl.com/Advanced-OpenGL/Instancing`
- TinyEngine example tree:
  `https://github.com/weigert/TinyEngine/tree/master/examples/0.0_Empty`

## Practical Translation For voxel-native

The current engine is Rust/Bevy/WGPU-oriented, so OpenGL examples should be
used as architecture pressure, not copied directly. The relevant ideas are:

- Keep greedy/chunk meshing for solid voxel terrain instead of instancing every
  cube. Instancing is useful for repeated props, particles, bot markers, and
  scenery objects, but per-voxel instancing would inflate draw counts and
  bandwidth for terrain.
- Reduce driver/API overhead by batching work: fewer mesh uploads, fewer tiny
  draw surfaces, and stable buffers for frequently changing editor previews.
- Move toward pooled or persistently reused mesh buffers for chunk rebuilds so
  flying/loading does not stall on allocation and upload churn.
- Add explicit render categories: terrain chunks, transparent/liquid/glass,
  editor previews/gizmos, bots/ships, particles, and scenery props. Each
  category should have a bounded update budget.
- Use LOD and impostor thinking for scenery beauty. Distant forests, city
  silhouettes, butterflies/particles, and skyline lights should not be full
  near-field geometry.
- Treat visual beauty as a streaming problem: the first loaded frame should
  show coherent terrain, sky, and silhouettes, then refine details without
  one-second hitches.

## Candidate Milestone

1. Instrument draw calls, mesh uploads, chunk rebuild queue length, and frame
   time around startup, fast flight, and bot proximity.
2. Cap per-frame mesh uploads and separate terrain rebuild budget from editor
   preview budget.
3. Pool chunk mesh buffers or introduce a staging allocator for rebuild output.
4. Add instanced rendering only for repeated non-terrain props where one mesh is
   reused many times.
5. Rework scenery generation to favor large readable silhouettes, color
   contrast, and low-cost detail layers before adding more geometry.
