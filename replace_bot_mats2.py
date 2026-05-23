import re

with open('src/bots.rs', 'r') as f:
    text = f.read()

replacements = [
    (r'fn companion_thruster_material.*?return handle\.clone\(\);\s*}\s*let handle = materials\.add\(StandardMaterial \{.*?base_color:.*?emissive:.*?metallic:.*?perceptual_roughness:.*?\}\);', 
     '''fn companion_thruster_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(handle) = &cache.companion_thruster_mat {
        return handle.clone();
    }
    // Deep orange/red scorched thruster with glow
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgb(0.2, 0.1, 0.05),
        emissive: LinearRgba::rgb(4.0, 1.5, 0.5),
        metallic: 0.8,
        perceptual_roughness: 0.9,
        ..default()
    });'''),
    
    (r'fn companion_rim_material.*?return handle\.clone\(\);\s*}\s*let handle = materials\.add\(StandardMaterial \{.*?base_color:.*?emissive:.*?metallic:.*?perceptual_roughness:.*?\}\);',
     '''fn companion_rim_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(handle) = &cache.companion_rim_mat {
        return handle.clone();
    }
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgb(0.1, 0.1, 0.12),
        emissive: LinearRgba::rgb(0.01, 0.01, 0.01),
        metallic: 0.95,
        perceptual_roughness: 0.82,
        ..default()
    });''')
]

for old, new in replacements:
    text, num = re.subn(old, new, text, flags=re.DOTALL)
    print(f"Replaced {num} occurrences")

with open('src/bots.rs', 'w') as f:
    f.write(text)
