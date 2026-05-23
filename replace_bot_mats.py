import re

with open('src/bots.rs', 'r') as f:
    text = f.read()

replacements = [
    (r'fn companion_aura_shell_material.*?return h\.clone\(\);\s*}\s*//.*?\s*let h = materials\.add\(StandardMaterial \{.*?base_color:.*?emissive:.*?metallic:.*?perceptual_roughness:.*?\}\);', 
     '''fn companion_aura_shell_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_aura_shell {
        return h.clone();
    }
    // Star Wars gritty realistic imperial white/grey
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.55, 0.55, 0.55),
        emissive: LinearRgba::rgb(0.01, 0.01, 0.01),
        metallic: 0.85,
        perceptual_roughness: 0.65,
        reflectance: 0.3,
        ..default()
    });'''),
    
    (r'fn companion_bolt_shell_material.*?return h\.clone\(\);\s*}\s*//.*?\s*let h = materials\.add\(StandardMaterial \{.*?base_color:.*?emissive:.*?metallic:.*?perceptual_roughness:.*?reflectance:.*?\}\);',
     '''fn companion_bolt_shell_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_bolt_shell {
        return h.clone();
    }
    // Star Wars realistic rusted astromech yellow/orange.
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.48, 0.35, 0.10),
        emissive: LinearRgba::rgb(0.02, 0.01, 0.0),
        metallic: 0.9,
        perceptual_roughness: 0.8,
        reflectance: 0.2,
        ..default()
    });'''),

    (r'fn companion_trim_material.*?return h\.clone\(\);\s*}\s*let h = materials\.add\(StandardMaterial \{.*?base_color:.*?emissive:.*?metallic:.*?perceptual_roughness:.*?\}\);',
     '''fn companion_trim_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_trim {
        return h.clone();
    }
    // Highly worn, dark structural metal.
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.08, 0.08, 0.09),
        emissive: LinearRgba::rgb(0.005, 0.005, 0.005),
        metallic: 1.0,
        perceptual_roughness: 0.85,
        ..default()
    });'''),

    (r'fn companion_ear_material.*?return h\.clone\(\);\s*}\s*let h = materials\.add\(StandardMaterial \{.*?base_color:.*?emissive:.*?metallic:.*?perceptual_roughness:.*?\}\);',
     '''fn companion_ear_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_ear {
        return h.clone();
    }
    // Scratched, oxidized dark metal.
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.12, 0.12, 0.12),
        emissive: LinearRgba::rgb(0.0, 0.0, 0.0),
        metallic: 0.95,
        perceptual_roughness: 0.75,
        ..default()
    });'''),

    (r'fn companion_visor_material.*?return h\.clone\(\);\s*}\s*let h = materials\.add\(StandardMaterial \{.*?base_color:.*?emissive:.*?metallic:.*?perceptual_roughness:.*?reflectance:.*?\}\);',
     '''fn companion_visor_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_visor {
        return h.clone();
    }
    // Dark, glossy black astromech sensor visor
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.01, 0.01, 0.01),
        emissive: LinearRgba::rgb(0.005, 0.005, 0.005),
        metallic: 0.8,
        perceptual_roughness: 0.1,
        reflectance: 0.9,
        ..default()
    });'''),

    (r'fn companion_shell_material.*?return handle\.clone\(\);\s*}\s*let handle = materials\.add\(StandardMaterial \{.*?base_color:.*?emissive:.*?metallic:.*?perceptual_roughness:.*?\}\);',
     '''fn companion_shell_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(handle) = &cache.companion_shell {
        return handle.clone();
    }
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgb(0.4, 0.4, 0.42),
        emissive: LinearRgba::rgb(0.05, 0.05, 0.05),
        metallic: 0.85,
        perceptual_roughness: 0.7,
        ..default()
    });''')
]

for old, new in replacements:
    text, num = re.subn(old, new, text, flags=re.DOTALL)
    print(f"Replaced {num} occurrences")

with open('src/bots.rs', 'w') as f:
    f.write(text)
