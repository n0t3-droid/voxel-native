//! Procedural texture generation + in-engine texture library.
//!
//! The game runs **without shipping any PNG files** by default: at startup
//! we bake a high-detail seamless universal-grain texture and a set of
//! per-block photorealistic *swatches* that the Texture Viewer can preview
//! and export to disk.
//!
//! Override pipeline:
//!   * If `./textures/universal_grain.png` exists at startup, we load it
//!     INSTEAD of baking the procedural one. Drop in a real photo-sourced
//!     PBR-style tile here to make every surface in the world feel
//!     photographic.
//!   * Per-block swatches are baked on demand for the Texture Viewer.
//!
//! The procedural pipeline is a blend of:
//!   * 6-octave domain-warped Perlin FBM (large shapes, pores, pockmarks)
//!   * Pseudo-Worley ridge noise          (crack / grout network, aggregate)
//!   * Anisotropic stretched Perlin       (wood grain, wind-drift, strata)
//!   * High-frequency speckle + sparkle   (pixel-level realism)

use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{
    Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::render::texture::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use noise::{NoiseFn, Perlin};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::blocks::{BlockType, MaterialId, CUSTOM_MATERIAL_BASE};
use crate::vegetation::VegetationSpecies;

/// Folder (relative to cwd) from which the engine loads user-supplied
/// texture overrides and into which the Texture Viewer exports PNGs.
pub const TEX_DIR: &str = "textures";
pub const MATERIAL_DIR: &str = "textures/materials";

/// Built-in swatches are kept modest enough for low-end GPUs, but large
/// enough that the repeated terrain texture survives mip/downsample blending.
pub const BUILTIN_SWATCH_SIZE: u32 = 128;

/// A process can remember this many distinct custom source identities. Retired
/// identities deliberately keep their MaterialId as a tombstone: reassigning
/// that id to another PNG would make old voxels change appearance mid-remesh.
const MAX_CUSTOM_MATERIAL_IDENTITIES: usize = 4_096;
/// Custom reload is a cold path, but it still needs a hard input/work budget.
const MAX_CUSTOM_PNG_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CUSTOM_TOTAL_PNG_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CUSTOM_PNG_EDGE_PIXELS: u32 = 4_096;
/// Counts every direct entry before extension filtering, so a directory full
/// of unrelated files cannot make reload work unbounded.
const MAX_CUSTOM_MATERIAL_DIRECTORY_ENTRIES: usize = 8_192;
/// Edited chunks persist raw `u16` MaterialIds without a source registry. New
/// custom sources therefore declare an explicit slot in their filename:
/// `material-32768__display-name.png`; list order never assigns it.
///
/// This is deliberately explicit instead of a truncated filename hash, and
/// duplicate declarations are rejected transactionally. It is only a slot
/// convention, not a persisted binding: historical list-derived saves could
/// use any saturated u16, and same-slot content reuse after restart cannot be
/// detected without a per-world catalog.
const DURABLE_CUSTOM_MATERIAL_BASE: MaterialId = 32_768;
const DURABLE_CUSTOM_FILENAME_PREFIX: &str = "material-";
/// Exact ceiling for the concatenated mip payloads of all active custom
/// `Image::data` vectors after a successful reload.
const MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;
const CUSTOM_TOMBSTONE_RGBA: [u8; 4] = [255, 0, 255, 255];
const CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES: usize = CUSTOM_TOMBSTONE_RGBA.len();
const UNRESOLVED_CUSTOM_IMAGE_PAYLOAD_BYTES: usize = CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES;
/// Every historical identity owns one image AssetId. Inactive identities hold
/// exactly one RGBA texel instead of their former full mip chain.
const MAX_CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES: usize =
    MAX_CUSTOM_MATERIAL_IDENTITIES * CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES;
/// Exact custom `Image::data` ceiling resident in Bevy after a successful
/// reload, including the one fixed unresolved-id magenta image. Other built-in
/// images are a separate fixed population.
const MAX_CUSTOM_RESIDENT_IMAGE_PAYLOAD_BYTES: usize = MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES
    + MAX_CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES
    + UNRESOLVED_CUSTOM_IMAGE_PAYLOAD_BYTES;
/// Exact peak for custom `Image::data` owned by Bevy plus the fully prepared
/// candidate transaction: old active payload is not wished away while the new
/// candidate is decoded. Compressed input (<=64 MiB) and image-decoder scratch
/// are separately bounded and intentionally not mislabeled as Image payload.
const MAX_CUSTOM_RELOAD_TRANSIENT_IMAGE_PAYLOAD_BYTES: usize =
    MAX_CUSTOM_RESIDENT_IMAGE_PAYLOAD_BYTES + MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES;
/// The PNG decoder receives this aggregate allocation budget. PNG supports the
/// image crate limit; the output image counts against it. The mip builder then
/// moves (rather than clones) that RGBA output and grows the chain in place.
const MAX_CUSTOM_PNG_DECODER_ALLOCATION_BYTES: usize = MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES;
/// Conservative, explicit CPU byte ceiling while a transaction is prepared:
/// old Bevy Image payload + staged/new pixel payload + PNG decoder allocations
/// + one bounded compressed source. Allocator bookkeeping/capacity rounding is
/// not pixel payload and is intentionally excluded from this exact sum.
const MAX_CUSTOM_RELOAD_DECLARED_CPU_BYTES: usize = MAX_CUSTOM_RELOAD_TRANSIENT_IMAGE_PAYLOAD_BYTES
    + MAX_CUSTOM_PNG_DECODER_ALLOCATION_BYTES
    + MAX_CUSTOM_PNG_SOURCE_BYTES as usize;

#[derive(Clone, Copy, Debug, PartialEq)]
struct TerrainMaterialProfile {
    base_alpha: f32,
    perceptual_roughness: f32,
    reflectance: f32,
    metallic: f32,
    alpha_mode: AlphaMode,
}

#[derive(Resource, Default)]
pub struct MaterialLibrary {
    pub handles: BTreeMap<MaterialId, Handle<StandardMaterial>>,
    pub vegetation_handles: BTreeMap<MaterialId, Handle<crate::vegetation::VegetationMaterial>>,
    pub names: BTreeMap<MaterialId, String>,
    pub custom_ids: Vec<MaterialId>,
    pub reload_requested: bool,
    pub status: String,
    /// One fixed, in-place-updated magenta material for unresolved raw custom
    /// ids. It is deliberately outside `handles`, so every explicit u16 slot
    /// remains available and exact registered handles still win.
    unresolved_custom_handle: Option<Handle<StandardMaterial>>,
    /// All explicit custom slots seen in this process, including currently
    /// absent files. Values are canonical `material-id:<u16>` tokens, not
    /// filenames. This prevents list-order drift, but it is not a persisted
    /// content/source catalog and cannot detect semantic slot reuse at restart.
    custom_sources: BTreeMap<MaterialId, String>,
    /// Exact current custom Image::data accounting, exposed to unit tests and
    /// status text without rescanning Assets at runtime.
    custom_active_image_payload_bytes: usize,
    custom_tombstone_image_payload_bytes: usize,
}

struct PreparedCustomMaterial {
    id: MaterialId,
    source_key: String,
    name: String,
    image: Image,
}

#[derive(Debug)]
struct OpenedCustomPng {
    file: File,
    opened_len: u64,
    opened_modified: std::time::SystemTime,
}

struct PlannedCustomMaterial {
    id: MaterialId,
    material: PreparedCustomMaterial,
}

struct CustomMaterialPlan {
    materials: Vec<PlannedCustomMaterial>,
    sources: BTreeMap<MaterialId, String>,
}

/// Replace an asset in place whenever the library already owns a handle.
/// `Assets::insert` preserves the AssetId and emits `AssetEvent::Modified`, so
/// entities that still carry the old handle observe the new value immediately.
fn replace_or_add_asset<A: Asset>(
    assets: &mut Assets<A>,
    previous: Option<&Handle<A>>,
    replacement: A,
) -> Handle<A> {
    if let Some(handle) = previous {
        assets.insert(handle.id(), replacement);
        handle.clone()
    } else {
        assets.add(replacement)
    }
}

fn custom_standard_material(image: Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(image),
        perceptual_roughness: 1.0,
        reflectance: 0.04,
        ..default()
    }
}

fn custom_tombstone_image() -> Image {
    let image = make_repeating_image(1, 1, CUSTOM_TOMBSTONE_RGBA.to_vec());
    debug_assert_eq!(image.data.len(), CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES);
    image
}

fn rgba8_mip_payload_bytes(mut width: u32, mut height: u32) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    let mut total = 0usize;
    loop {
        let level = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        total = total.checked_add(level)?;
        if width == 1 && height == 1 {
            return Some(total);
        }
        width = (width / 2).max(1);
        height = (height / 2).max(1);
    }
}

fn durable_custom_source_key(id: MaterialId) -> String {
    format!("material-id:{id}")
}

fn parse_durable_custom_filename(file_name: &str) -> Result<(MaterialId, String, String), String> {
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "Custom-PNG besitzt keinen gueltigen Namen".to_string())?;
    let Some(declaration) = stem.strip_prefix(DURABLE_CUSTOM_FILENAME_PREFIX) else {
        return Err(format!(
            "Custom-PNG braucht eine dauerhafte Id: material-{DURABLE_CUSTOM_MATERIAL_BASE}__name.png"
        ));
    };
    let Some((id_text, display_name)) = declaration.split_once("__") else {
        return Err(format!(
            "Custom-PNG braucht das Format material-{DURABLE_CUSTOM_MATERIAL_BASE}__name.png"
        ));
    };
    if id_text.len() != 5 || !id_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(
            "dauerhafte Custom-MaterialId muss genau fuenf Dezimalstellen haben".to_string(),
        );
    }
    let id = id_text
        .parse::<MaterialId>()
        .map_err(|_| "dauerhafte Custom-MaterialId liegt ausserhalb u16".to_string())?;
    if id < DURABLE_CUSTOM_MATERIAL_BASE {
        return Err(format!(
            "dauerhafte Custom-MaterialId muss mindestens {DURABLE_CUSTOM_MATERIAL_BASE} sein"
        ));
    }
    if display_name.is_empty() {
        return Err("Custom-PNG braucht einen Anzeigenamen nach __".to_string());
    }
    Ok((id, durable_custom_source_key(id), display_name.to_string()))
}

fn plan_custom_materials(
    mut prepared: Vec<PreparedCustomMaterial>,
    previous_sources: &BTreeMap<MaterialId, String>,
) -> Result<CustomMaterialPlan, String> {
    if prepared.len() > MAX_CUSTOM_MATERIAL_IDENTITIES {
        return Err(format!(
            "mehr als {MAX_CUSTOM_MATERIAL_IDENTITIES} aktive Custom-Materialien"
        ));
    }
    if previous_sources.len() > MAX_CUSTOM_MATERIAL_IDENTITIES {
        return Err("Custom-Identitaetsregister ist ausserhalb des Limits".to_string());
    }
    let prepared_payload = prepared.iter().try_fold(0usize, |total, material| {
        total.checked_add(material.image.data.len())
    });
    if prepared_payload.map_or(true, |bytes| bytes > MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES) {
        return Err("vorbereiteter Custom-Bildpayload ist ausserhalb des Limits".to_string());
    }

    prepared.sort_by_key(|material| material.id);
    if prepared.iter().any(|material| {
        material.id < DURABLE_CUSTOM_MATERIAL_BASE
            || material.source_key != durable_custom_source_key(material.id)
    }) {
        return Err("ungueltige oder mehrdeutige dauerhafte Custom-Identitaet".to_string());
    }
    if prepared.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err("doppelte dauerhafte Custom-MaterialId".to_string());
    }

    for (&id, source) in previous_sources {
        if id < DURABLE_CUSTOM_MATERIAL_BASE || source != &durable_custom_source_key(id) {
            // Legacy/list-derived ids cannot be reconstructed from raw u16
            // chunk saves. Refuse them instead of silently preserving a
            // potentially wrong filename-to-id assignment.
            return Err(
                "mehrdeutiger historischer Custom-Eintrag; dauerhafte Id fehlt".to_string(),
            );
        }
    }

    let new_identity_count = prepared
        .iter()
        .filter(|material| !previous_sources.contains_key(&material.id))
        .count();
    if previous_sources
        .len()
        .checked_add(new_identity_count)
        .map_or(true, |count| count > MAX_CUSTOM_MATERIAL_IDENTITIES)
    {
        return Err("Custom-Identitaetsbudget erschoepft".to_string());
    }

    let mut sources = previous_sources.clone();
    let mut materials = Vec::with_capacity(prepared.len());
    for material in prepared {
        let id = material.id;
        if let Some(previous_source) = sources.get(&id) {
            if previous_source != &material.source_key {
                return Err("dauerhafte Custom-MaterialId wurde neu zugeordnet".to_string());
            }
        } else {
            sources.insert(id, material.source_key.clone());
        }
        materials.push(PlannedCustomMaterial { id, material });
    }

    Ok(CustomMaterialPlan { materials, sources })
}

fn prepare_custom_materials() -> Result<Vec<PreparedCustomMaterial>, String> {
    std::fs::create_dir_all(MATERIAL_DIR)
        .map_err(|_| "Custom-Verzeichnis konnte nicht erstellt werden".to_string())?;
    let read = std::fs::read_dir(MATERIAL_DIR)
        .map_err(|_| "Custom-Verzeichnis konnte nicht gelesen werden".to_string())?;

    let mut scanned_entries = 0usize;
    let mut sources: Vec<(MaterialId, String, String, PathBuf)> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|_| "Custom-Verzeichniseintrag ist unlesbar".to_string())?;
        count_custom_directory_entry(&mut scanned_entries)?;
        let path = entry.path();
        let is_png = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("png"));
        if !is_png {
            continue;
        }
        if sources.len() >= MAX_CUSTOM_MATERIAL_IDENTITIES {
            return Err(format!(
                "mehr als {MAX_CUSTOM_MATERIAL_IDENTITIES} PNG-Dateien"
            ));
        }
        let file_name_os = entry.file_name();
        let file_name = file_name_os
            .to_str()
            .ok_or_else(|| "Custom-PNG-Dateiname ist nicht UTF-8".to_string())?;
        let (id, source_key, name) = parse_durable_custom_filename(file_name)?;
        sources.push((id, source_key, name, path));
    }

    sources.sort_by_key(|source| source.0);
    if sources.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("doppelte dauerhafte Custom-MaterialId".to_string());
    }

    // Decode the whole candidate set before mutating Assets. One bad file
    // therefore rejects the reload transaction instead of publishing a
    // partially shifted MaterialId table.
    let mut prepared = Vec::with_capacity(sources.len());
    let mut total_image_bytes = 0usize;
    let mut total_source_bytes = 0u64;
    for (id, source_key, name, path) in sources {
        let remaining_payload = MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES
            .checked_sub(total_image_bytes)
            .ok_or_else(|| "Custom-PNG-Gesamtbudget uebergelaufen".to_string())?;
        // The loader probes dimensions and exact mip bytes before allocating
        // the decoded candidate. The staged Image payload can therefore never
        // overshoot its limit by one large final PNG before being rejected.
        let source = open_custom_png_once(&path)?;
        total_source_bytes = checked_custom_source_total(total_source_bytes, source.opened_len)?;
        let image = load_bounded_custom_png(source, remaining_payload)?;
        total_image_bytes += image.data.len();
        prepared.push(PreparedCustomMaterial {
            id,
            source_key,
            name,
            image,
        });
    }
    Ok(prepared)
}

fn checked_custom_source_total(current: u64, next: u64) -> Result<u64, String> {
    let total = current
        .checked_add(next)
        .ok_or_else(|| "Custom-PNG-Gesamtquellbudget ist uebergelaufen".to_string())?;
    if total > MAX_CUSTOM_TOTAL_PNG_SOURCE_BYTES {
        return Err(format!(
            "Custom-PNGs ueberschreiten zusammen {} MiB Quelldaten",
            MAX_CUSTOM_TOTAL_PNG_SOURCE_BYTES / (1024 * 1024)
        ));
    }
    Ok(total)
}

fn count_custom_directory_entry(scanned_entries: &mut usize) -> Result<(), String> {
    *scanned_entries = scanned_entries
        .checked_add(1)
        .ok_or_else(|| "Custom-Verzeichniseintraege sind uebergelaufen".to_string())?;
    if *scanned_entries > MAX_CUSTOM_MATERIAL_DIRECTORY_ENTRIES {
        return Err(format!(
            "mehr als {MAX_CUSTOM_MATERIAL_DIRECTORY_ENTRIES} Verzeichniseintraege"
        ));
    }
    Ok(())
}

fn custom_image_decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_CUSTOM_PNG_EDGE_PIXELS);
    limits.max_image_height = Some(MAX_CUSTOM_PNG_EDGE_PIXELS);
    limits.max_alloc = Some(MAX_CUSTOM_PNG_DECODER_ALLOCATION_BYTES as u64);
    limits
}

fn metadata_is_regular_non_reparse(metadata: &Metadata) -> bool {
    if !metadata.file_type().is_file() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

fn metadata_has_same_file_identity(left: &Metadata, right: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Stable Rust does not expose the Windows file index. This is a
        // conservative stable fingerprint around the one opened handle; the
        // reparse bit is checked separately. Same-size/same-timestamp hostile
        // replacement remains a documented platform limit.
        return left.file_attributes() == right.file_attributes()
            && left.creation_time() == right.creation_time()
            && left.last_write_time() == right.last_write_time()
            && left.file_size() == right.file_size();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        return left.dev() == right.dev() && left.ino() == right.ino();
    }
    #[cfg(not(any(unix, windows)))]
    {
        left.len() == right.len() && left.modified().ok() == right.modified().ok()
    }
}

fn validate_custom_png_metadata(metadata: &Metadata) -> Result<(), String> {
    if !metadata_is_regular_non_reparse(metadata) {
        return Err("Custom-PNG ist keine regulaere Datei oder ist ein Reparse-Link".to_string());
    }
    if metadata.len() > MAX_CUSTOM_PNG_SOURCE_BYTES {
        return Err(format!(
            "Custom-PNG ueberschreitet {} MiB",
            MAX_CUSTOM_PNG_SOURCE_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

fn open_custom_png_once(path: &Path) -> Result<OpenedCustomPng, String> {
    // The path is inspected on both sides of the one data-file open. Platform
    // identity/fingerprint equality with the opened handle detects ordinary
    // replace/link races; all later reads and checks use only that handle.
    let before = std::fs::symlink_metadata(path)
        .map_err(|_| "Custom-PNG-Metadaten sind unlesbar".to_string())?;
    validate_custom_png_metadata(&before)?;
    let file =
        File::open(path).map_err(|_| "Custom-PNG konnte nicht geoeffnet werden".to_string())?;
    let opened = file
        .metadata()
        .map_err(|_| "Custom-PNG-Handle-Metadaten sind unlesbar".to_string())?;
    validate_custom_png_metadata(&opened)?;
    let after = std::fs::symlink_metadata(path)
        .map_err(|_| "Custom-PNG-Metadaten sind nach dem Oeffnen unlesbar".to_string())?;
    validate_custom_png_metadata(&after)?;
    if !metadata_has_same_file_identity(&before, &opened)
        || !metadata_has_same_file_identity(&opened, &after)
    {
        return Err("Custom-PNG wurde beim Oeffnen ersetzt oder umgeleitet".to_string());
    }
    let opened_modified = opened
        .modified()
        .map_err(|_| "Custom-PNG-Aenderungszeit ist unlesbar".to_string())?;
    if before.len() != opened.len()
        || after.len() != opened.len()
        || before.modified().ok() != Some(opened_modified)
        || after.modified().ok() != Some(opened_modified)
    {
        return Err("Custom-PNG hat sich beim Oeffnen geaendert".to_string());
    }
    Ok(OpenedCustomPng {
        file,
        opened_len: opened.len(),
        opened_modified,
    })
}

fn read_bounded<R: Read>(
    reader: &mut R,
    maximum_bytes: usize,
    capacity_hint: usize,
) -> Result<Vec<u8>, String> {
    let read_limit = maximum_bytes
        .checked_add(1)
        .ok_or_else(|| "Custom-PNG-Leselimit ist uebergelaufen".to_string())?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity_hint.min(read_limit))
        .map_err(|_| "Custom-PNG-Lesepuffer konnte nicht reserviert werden".to_string())?;
    reader
        .take(read_limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Custom-PNG konnte nicht gelesen werden".to_string())?;
    if bytes.len() > maximum_bytes {
        return Err("Custom-PNG hat das harte Leselimit ueberschritten".to_string());
    }
    Ok(bytes)
}

fn load_bounded_custom_png(
    mut source: OpenedCustomPng,
    remaining_payload_bytes: usize,
) -> Result<Image, String> {
    let source_limit = usize::try_from(MAX_CUSTOM_PNG_SOURCE_BYTES)
        .map_err(|_| "Custom-PNG-Leselimit passt nicht in usize".to_string())?;
    let capacity_hint = usize::try_from(source.opened_len)
        .map_err(|_| "Custom-PNG-Dateigroesse passt nicht in usize".to_string())?;
    let bytes = read_bounded(&mut source.file, source_limit, capacity_hint)?;
    let after = source
        .file
        .metadata()
        .map_err(|_| "Custom-PNG-Handle-Metadaten sind nach dem Lesen unlesbar".to_string())?;
    validate_custom_png_metadata(&after)?;
    if after.len() != source.opened_len
        || bytes.len() as u64 != source.opened_len
        || after.modified().ok() != Some(source.opened_modified)
    {
        return Err(
            "Custom-PNG ist waehrend des bounded Reads gewachsen oder geaendert".to_string(),
        );
    }

    decode_custom_png(bytes, remaining_payload_bytes)
}

fn decode_custom_png(bytes: Vec<u8>, remaining_payload_bytes: usize) -> Result<Image, String> {
    let mut probe = image::ImageReader::with_format(
        std::io::Cursor::new(bytes.as_slice()),
        image::ImageFormat::Png,
    );
    probe.limits(custom_image_decode_limits());
    let (width, height) = probe.into_dimensions().map_err(|_| {
        "Custom-PNG-Header ist ungueltig oder ueber dem Dimensionslimit".to_string()
    })?;
    let expected_payload = rgba8_mip_payload_bytes(width, height)
        .ok_or_else(|| "Custom-PNG-Payloadgroesse ist ungueltig".to_string())?;
    if expected_payload > remaining_payload_bytes {
        return Err(format!(
            "Custom-PNGs ueberschreiten {} MiB dekodierte Bilddaten",
            MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES / (1024 * 1024)
        ));
    }

    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    reader.limits(custom_image_decode_limits());
    let decoded = reader
        .decode()
        .map_err(|_| "Custom-PNG ist ungueltig oder ueberschreitet das Dekodierlimit".to_string())?
        .into_rgba8();
    if decoded.dimensions() != (width, height) {
        return Err("Custom-PNG-Dimensionen sind waehrend des Dekodierens instabil".to_string());
    }
    let image = make_repeating_image(width, height, decoded.into_raw());
    if image.data.len() != expected_payload {
        return Err("Custom-PNG-Mip-Payload weicht von der Vorpruefung ab".to_string());
    }
    Ok(image)
}

pub(crate) fn terrain_alpha_mode_for_block(block: BlockType) -> AlphaMode {
    if block == BlockType::Lava {
        // The process-wide render policy is `Msaa::Off`. In Bevy 0.14,
        // AlphaToCoverage without MSAA becomes a binary alpha mask, so Lava's
        // authored 0.88 alpha was opaque in practice already. State that
        // depth-stable policy explicitly instead of advertising unsupported
        // translucency for a surface that can cover much of the screen.
        return AlphaMode::Opaque;
    }
    let alpha = block.color().to_srgba().alpha;
    if alpha < 0.99 {
        // Chunk terrain can fill the whole screen. Alpha-to-coverage keeps
        // glass/ice/crystal out of Bevy's sorted Blend pass.
        AlphaMode::AlphaToCoverage
    } else {
        AlphaMode::Opaque
    }
}

fn terrain_material_profile(block: BlockType) -> TerrainMaterialProfile {
    let alpha = block.color().to_srgba().alpha;
    if block == BlockType::Water {
        // Water opacity comes from the mesh's voxel tint. The material and
        // procedural ripple albedo stay opaque, avoiding alpha being
        // multiplied by texture, material and vertex color.
        TerrainMaterialProfile {
            base_alpha: 1.0,
            perceptual_roughness: 0.18,
            // Bevy maps this parameter to dielectric F0 as 0.16*r^2.
            // r=0.357 therefore gives 2.04%, matching the normal-incidence
            // Fresnel reflectance of an air/water interface at IOR 1.333.
            reflectance: 0.357,
            metallic: 0.0,
            alpha_mode: AlphaMode::AlphaToCoverage,
        }
    } else if block == BlockType::Lava {
        TerrainMaterialProfile {
            base_alpha: 1.0,
            perceptual_roughness: 1.0,
            reflectance: 0.05,
            metallic: 0.0,
            alpha_mode: AlphaMode::Opaque,
        }
    } else if matches!(
        block,
        BlockType::Leaves
            | BlockType::JungleLeaves
            | BlockType::BlossomLeaves
            | BlockType::SakuraPetals
    ) {
        // A restrained broad highlight keeps crowns readable as volumes as
        // the wind changes their facing. The values stay rough and diffuse;
        // this is waxy leaf response, not wet plastic.
        TerrainMaterialProfile {
            base_alpha: alpha,
            perceptual_roughness: if matches!(
                block,
                BlockType::BlossomLeaves | BlockType::SakuraPetals
            ) {
                0.74
            } else {
                0.82
            },
            // A non-metallic cuticle sits in the ordinary dielectric band;
            // 0.42 maps to about 2.82% normal-incidence F0 in Bevy.
            reflectance: 0.42,
            metallic: 0.0,
            // The procedural albedo contains a restrained binary pore mask.
            // Alpha mask is temporally stable, works with the vegetation
            // extension, and avoids sorting thousands of canopy surfaces.
            alpha_mode: AlphaMode::Mask(0.42),
        }
    } else {
        // Scalar optical families give the procedural albedos distinct,
        // physically legible responses without adding textures, tangents,
        // vertices, entities or draw calls. Reflectance is Bevy's remapped
        // dielectric control (F0 = 0.16*r^2), not a direct percentage.
        let (perceptual_roughness, reflectance, metallic) = match block {
            // Loose mineral/organic ground: broad, low-energy highlights.
            BlockType::Dirt => (0.96, 0.36, 0.0),
            BlockType::Grass
            | BlockType::TundraGrass
            | BlockType::SavannaGrass
            | BlockType::AlienMoss => (0.90, 0.38, 0.0),
            BlockType::Sand | BlockType::RedSand | BlockType::GlowSand => (0.92, 0.40, 0.0),

            // Rock remains rough, but no longer shares soil's chalk-flat
            // response. Polished zen stone is deliberately the smoothest.
            BlockType::Gravel | BlockType::Bedrock | BlockType::Basalt => (0.88, 0.46, 0.0),
            BlockType::Stone
            | BlockType::RedStone
            | BlockType::MesaClay
            | BlockType::MossStone
            | BlockType::Limestone
            | BlockType::BoneRock => (0.78, 0.48, 0.0),
            BlockType::ZenStone => (0.58, 0.50, 0.0),

            // Fibrous surfaces retain elongated-looking soft highlights even
            // though the current bounded mesh contract has no tangents.
            BlockType::Wood | BlockType::Bamboo | BlockType::TatamiMat => (0.76, 0.42, 0.0),
            BlockType::ShojiPaper => (0.93, 0.36, 0.0),
            BlockType::RoofTile => (0.64, 0.50, 0.0),

            // Snow is diffuse; ice and dielectric crystals carry tight
            // highlights. Transparent blocks keep their existing depth-stable
            // alpha policy rather than entering the sorted Blend path.
            BlockType::Snow => (0.82, 0.42, 0.0),
            BlockType::Ice => (0.20, 0.46, 0.0),
            BlockType::Crystal
            | BlockType::CockpitGlass
            | BlockType::LuminiteCrystal
            | BlockType::NeonGlass => (0.12, 0.50, 0.0),
            BlockType::NeonCyan | BlockType::NeonMagenta | BlockType::NeonAmber => {
                (0.22, 0.48, 0.0)
            }

            // Manufactured and ore-bearing surfaces are the only built-in
            // conductors. Metallic remains bounded below one so their baked
            // albedo and bounded emission still retain readable mid-tones.
            BlockType::ShipHullDark => (0.42, 0.52, 0.72),
            BlockType::ShipHullAlloy => (0.28, 0.56, 0.90),
            BlockType::MagnetiteOre => (0.54, 0.50, 0.58),
            BlockType::IridiumVein => (0.30, 0.54, 0.78),
            BlockType::EngineCore => (0.34, 0.50, 0.62),

            // Warm lamp ceramic is rough while its light output remains under
            // the existing emission authority.
            BlockType::ShojiLamp => (0.70, 0.46, 0.0),

            // Air is never materialized; keep a finite fail-closed profile if
            // a diagnostic path nevertheless asks for it.
            BlockType::Air => (1.0, 0.0, 0.0),
            // Water, lava and foliage were handled above.
            BlockType::Water
            | BlockType::Lava
            | BlockType::Leaves
            | BlockType::JungleLeaves
            | BlockType::BlossomLeaves
            | BlockType::SakuraPetals => unreachable!("special material handled above"),
        };
        TerrainMaterialProfile {
            base_alpha: alpha,
            perceptual_roughness,
            reflectance,
            metallic,
            alpha_mode: terrain_alpha_mode_for_block(block),
        }
    }
}

/// Material-space emission for built-in terrain.
///
/// Lava already receives HDR vertex colour through the explicit
/// [`crate::voxel_budget::EmissionBudget`] path in the mesher. Giving the same
/// large, connected surface an additional uniform material emissive term made
/// that budget non-authoritative and erased the procedural swatch under ACES.
/// Other emissive blocks intentionally retain their established material term.
fn terrain_material_emissive(block: BlockType) -> LinearRgba {
    if block == BlockType::Lava || !block.is_emissive() {
        return LinearRgba::BLACK;
    }

    let lin = block.color().to_linear();
    LinearRgba::rgb(
        lin.red * 3.2 + 0.35,
        lin.green * 3.2 + 0.35,
        lin.blue * 3.2 + 0.35,
    )
}

impl MaterialLibrary {
    pub fn rebuild(
        &mut self,
        materials: &mut Assets<StandardMaterial>,
        vegetation_materials: &mut Assets<crate::vegetation::VegetationMaterial>,
        images: &mut Assets<Image>,
    ) {
        let custom_materials = prepare_custom_materials();
        self.rebuild_prepared(
            materials,
            vegetation_materials,
            images,
            BUILTIN_SWATCH_SIZE,
            custom_materials,
        );
    }

    fn rebuild_prepared(
        &mut self,
        materials: &mut Assets<StandardMaterial>,
        vegetation_materials: &mut Assets<crate::vegetation::VegetationMaterial>,
        images: &mut Assets<Image>,
        swatch_size: u32,
        custom_materials: Result<Vec<PreparedCustomMaterial>, String>,
    ) {
        // Do not clear-and-add here. Chunks remesh under a fixed budget, so
        // some visible entities keep their old handles for multiple frames.
        // Holding the previous maps also keeps every generated AssetId alive
        // while its value is replaced in place below.
        let previous_handles = std::mem::take(&mut self.handles);
        let previous_vegetation_handles = std::mem::take(&mut self.vegetation_handles);
        let previous_names = std::mem::take(&mut self.names);
        let previous_custom_ids = std::mem::take(&mut self.custom_ids);
        let previous_custom_sources = std::mem::take(&mut self.custom_sources);
        let previous_active_payload = self.custom_active_image_payload_bytes;
        let previous_tombstone_payload = self.custom_tombstone_image_payload_bytes;
        let previous_unresolved_custom_handle = self.unresolved_custom_handle.take();

        let mut next_handles = BTreeMap::new();
        let mut next_vegetation_handles = BTreeMap::new();
        let mut next_names = BTreeMap::new();
        let swatches = bake_all_block_swatches(swatch_size);
        let built_in_count = swatches.len();

        for swatch in swatches {
            let id = swatch.block as MaterialId;
            // Standard and vegetation materials intentionally share one image
            // handle. Reusing it also prevents texture population growth on
            // repeated reloads, not just material population growth.
            let previous_image = previous_handles
                .get(&id)
                .and_then(|handle| materials.get(handle))
                .and_then(|material| material.base_color_texture.clone())
                .or_else(|| {
                    previous_vegetation_handles
                        .get(&id)
                        .and_then(|handle| vegetation_materials.get(handle))
                        .and_then(|material| material.base.base_color_texture.clone())
                });
            let image = replace_or_add_asset(
                images,
                previous_image.as_ref(),
                make_repeating_image(swatch.width, swatch.height, swatch.rgba),
            );
            let profile = terrain_material_profile(swatch.block);
            let foliage = matches!(
                swatch.block,
                BlockType::Leaves
                    | BlockType::JungleLeaves
                    | BlockType::BlossomLeaves
                    | BlockType::SakuraPetals
            );
            let emissive = terrain_material_emissive(swatch.block);
            let standard_material = StandardMaterial {
                base_color: Color::WHITE.with_alpha(profile.base_alpha),
                base_color_texture: Some(image),
                emissive,
                perceptual_roughness: profile.perceptual_roughness,
                reflectance: profile.reflectance,
                metallic: profile.metallic,
                alpha_mode: profile.alpha_mode,
                cull_mode: if foliage {
                    None
                } else {
                    Some(bevy::render::render_resource::Face::Back)
                },
                double_sided: foliage,
                ..default()
            };
            if let Some(extension) = crate::vegetation::VegetationWind::for_block(swatch.block) {
                let wind_handle = replace_or_add_asset(
                    vegetation_materials,
                    previous_vegetation_handles.get(&id),
                    ExtendedMaterial {
                        base: standard_material.clone(),
                        extension,
                    },
                );
                next_vegetation_handles.insert(id, wind_handle);
            }
            let handle =
                replace_or_add_asset(materials, previous_handles.get(&id), standard_material);
            next_handles.insert(id, handle);
            next_names.insert(id, swatch.name.to_string());
        }

        // Unknown raw custom ids (notably legacy save ids after a restart)
        // must fail visibly rather than silently becoming Stone. This single
        // material/image pair is replaced in place on every rebuild, keeping
        // its AssetIds and population fixed without occupying a MaterialId.
        let previous_unresolved_image = previous_unresolved_custom_handle
            .as_ref()
            .and_then(|handle| materials.get(handle))
            .and_then(|material| material.base_color_texture.clone());
        let unresolved_image = replace_or_add_asset(
            images,
            previous_unresolved_image.as_ref(),
            custom_tombstone_image(),
        );
        let unresolved_custom_handle = replace_or_add_asset(
            materials,
            previous_unresolved_custom_handle.as_ref(),
            custom_standard_material(unresolved_image),
        );
        self.unresolved_custom_handle = Some(unresolved_custom_handle);

        // Retired entries remain addressable tombstones for this process, so
        // chunks waiting for bounded remesh keep their AssetIds. Cross-restart
        // raw-id/source binding still needs a persisted world catalog.
        for id in previous_custom_sources.keys().copied() {
            if let Some(handle) = previous_handles.get(&id) {
                next_handles.insert(id, handle.clone());
            }
            if let Some(name) = previous_names.get(&id) {
                next_names.insert(id, name.clone());
            }
        }

        let custom_plan = custom_materials
            .and_then(|prepared| plan_custom_materials(prepared, &previous_custom_sources))
            .and_then(|plan| {
                let active_ids: BTreeSet<_> = previous_custom_ids.iter().copied().collect();
                if active_ids.len() != previous_custom_ids.len()
                    || active_ids
                        .iter()
                        .any(|id| !previous_custom_sources.contains_key(id))
                {
                    return Err("vorherige aktive Custom-Ids sind inkonsistent".to_string());
                }
                // Image AssetIds are part of the identity contract. Validate
                // every historical entry before mutating any custom asset so
                // success never silently allocates a replacement id.
                let mut measured_active_payload = 0usize;
                let mut measured_tombstone_payload = 0usize;
                for id in previous_custom_sources.keys() {
                    let handle = previous_handles
                        .get(id)
                        .ok_or_else(|| "historischer Custom-Materialhandle fehlt".to_string())?;
                    let material = materials
                        .get(handle)
                        .ok_or_else(|| "historisches Custom-Material fehlt".to_string())?;
                    let image = material
                        .base_color_texture
                        .as_ref()
                        .ok_or_else(|| "historischer Custom-Bildhandle fehlt".to_string())?;
                    let image = images
                        .get(image)
                        .ok_or_else(|| "historisches Custom-Bildasset fehlt".to_string())?;
                    if active_ids.contains(id) {
                        measured_active_payload = measured_active_payload
                            .checked_add(image.data.len())
                            .ok_or_else(|| {
                                "aktiver Custom-Bildpayload uebergelaufen".to_string()
                            })?;
                    } else {
                        if image.texture_descriptor.size.width != 1
                            || image.texture_descriptor.size.height != 1
                            || image.data.as_slice() != CUSTOM_TOMBSTONE_RGBA
                        {
                            return Err(
                                "historisches Custom-Tombstone ist inkonsistent".to_string()
                            );
                        }
                        measured_tombstone_payload = measured_tombstone_payload
                            .checked_add(image.data.len())
                            .ok_or_else(|| {
                                "Custom-Tombstone-Bildpayload uebergelaufen".to_string()
                            })?;
                    }
                }
                if measured_active_payload != previous_active_payload
                    || measured_tombstone_payload != previous_tombstone_payload
                    || measured_active_payload > MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES
                    || measured_tombstone_payload > MAX_CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES
                    || measured_active_payload.saturating_add(measured_tombstone_payload)
                        > MAX_CUSTOM_RESIDENT_IMAGE_PAYLOAD_BYTES
                {
                    return Err("vorherige Custom-Bildpayload-Bilanz ist inkonsistent".to_string());
                }
                Ok(plan)
            });
        let (custom_sources, custom_ids, active_payload, tombstone_payload, custom_status) =
            match custom_plan {
                Ok(plan) => {
                    let active_ids: Vec<_> =
                        plan.materials.iter().map(|material| material.id).collect();
                    let active_id_set: BTreeSet<_> = active_ids.iter().copied().collect();
                    let previous_active_id_set: BTreeSet<_> =
                        previous_custom_ids.iter().copied().collect();
                    let active_payload =
                        plan.materials.iter().try_fold(0usize, |total, material| {
                            total.checked_add(material.material.image.data.len())
                        });
                    let Some(active_payload) = active_payload else {
                        unreachable!("prepared custom payload was already checked")
                    };
                    debug_assert!(active_payload <= MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES);
                    debug_assert!(
                        previous_active_payload.saturating_add(previous_tombstone_payload)
                            <= MAX_CUSTOM_RESIDENT_IMAGE_PAYLOAD_BYTES
                    );
                    debug_assert!(
                        previous_active_payload
                            .saturating_add(previous_tombstone_payload)
                            .saturating_add(active_payload)
                            <= MAX_CUSTOM_RELOAD_TRANSIENT_IMAGE_PAYLOAD_BYTES
                    );

                    // Only a successful plan may retire assets. Replace each
                    // newly inactive source under its existing material and image
                    // AssetIds, releasing the old full mip payload immediately
                    // while old chunk handles continue to resolve deterministically.
                    for id in previous_active_id_set.difference(&active_id_set) {
                        let material_handle = previous_handles
                            .get(id)
                            .expect("validated historical custom material handle");
                        let image_handle = materials
                            .get(material_handle)
                            .and_then(|material| material.base_color_texture.clone())
                            .expect("validated historical custom image handle");
                        let tombstone_image = replace_or_add_asset(
                            images,
                            Some(&image_handle),
                            custom_tombstone_image(),
                        );
                        debug_assert_eq!(tombstone_image.id(), image_handle.id());
                        let tombstone_material = replace_or_add_asset(
                            materials,
                            Some(material_handle),
                            custom_standard_material(tombstone_image),
                        );
                        debug_assert_eq!(tombstone_material.id(), material_handle.id());
                        next_handles.insert(*id, tombstone_material);
                    }

                    for planned in plan.materials {
                        let previous_handle = previous_handles.get(&planned.id);
                        let previous_image = previous_handle
                            .and_then(|handle| materials.get(handle))
                            .and_then(|material| material.base_color_texture.clone());
                        let image = replace_or_add_asset(
                            images,
                            previous_image.as_ref(),
                            planned.material.image,
                        );
                        let handle = replace_or_add_asset(
                            materials,
                            previous_handle,
                            custom_standard_material(image),
                        );
                        next_handles.insert(planned.id, handle);
                        next_names.insert(planned.id, planned.material.name);
                    }
                    let loaded = active_ids.len();
                    let tombstone_payload = plan
                        .sources
                        .len()
                        .saturating_sub(active_id_set.len())
                        .saturating_mul(CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES);
                    debug_assert!(tombstone_payload <= MAX_CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES);
                    debug_assert!(
                        active_payload.saturating_add(tombstone_payload)
                            <= MAX_CUSTOM_RESIDENT_IMAGE_PAYLOAD_BYTES
                    );
                    (
                        plan.sources,
                        active_ids,
                        active_payload,
                        tombstone_payload,
                        format!(
                            "{loaded} custom aus ./{MATERIAL_DIR}; Bildpayload aktiv {active_payload} B, Tombstones {tombstone_payload} B; CPU-Reloadbudget {MAX_CUSTOM_RELOAD_DECLARED_CPU_BYTES} B"
                        ),
                    )
                }
                Err(error) => {
                    // Fail closed as one transaction: never publish a partial
                    // remapping when enumeration, identity validation, or one PNG
                    // decode fails. The last coherent active set stays visible.
                    let retained = previous_custom_ids.len();
                    (
                        previous_custom_sources,
                        previous_custom_ids,
                        previous_active_payload,
                        previous_tombstone_payload,
                        format!("Custom-Reload abgelehnt ({error}); {retained} vorherige behalten"),
                    )
                }
            };

        self.handles = next_handles;
        self.vegetation_handles = next_vegetation_handles;
        self.names = next_names;
        self.custom_ids = custom_ids;
        self.custom_sources = custom_sources;
        self.custom_active_image_payload_bytes = active_payload;
        self.custom_tombstone_image_payload_bytes = tombstone_payload;
        self.reload_requested = false;
        self.status = format!(
            "Materialien geladen: {built_in_count} built-in; {custom_status}; Slots 32768+: explizit, unbekannt/legacy -> Magenta; Neustart-Bindung ohne Weltkatalog nicht beweisbar"
        );
    }

    #[cfg(test)]
    pub(crate) fn rebuild_without_custom_for_test(
        &mut self,
        materials: &mut Assets<StandardMaterial>,
        vegetation_materials: &mut Assets<crate::vegetation::VegetationMaterial>,
        images: &mut Assets<Image>,
        swatch_size: u32,
    ) {
        self.rebuild_prepared(
            materials,
            vegetation_materials,
            images,
            swatch_size,
            Ok(Vec::new()),
        );
    }

    pub fn handle_for(&self, id: MaterialId) -> Option<Handle<StandardMaterial>> {
        if let Some(handle) = self.handles.get(&id) {
            return Some(handle.clone());
        }
        if id >= CUSTOM_MATERIAL_BASE {
            return self.unresolved_custom_handle.clone();
        }
        self.handles.get(&(BlockType::Stone as MaterialId)).cloned()
    }

    /// Resolve the fixed canonical material for an authoritative foliage
    /// species. Editable material IDs remain in the mesh key, but do not select
    /// the extension preset: doing so would let a custom/non-foliage material
    /// opt foliage out of wind or require an unbounded material cross-product.
    pub fn vegetation_handle_for_species(
        &self,
        species: VegetationSpecies,
    ) -> Option<Handle<crate::vegetation::VegetationMaterial>> {
        self.vegetation_handles
            .get(&(species.block() as MaterialId))
            .cloned()
    }

    #[allow(dead_code)]
    pub fn name_for(&self, id: MaterialId) -> String {
        self.names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("material_{id}"))
    }
}

// ---------------------------------------------------------------------------
// Universal grain -----------------------------------------------------------
// ---------------------------------------------------------------------------

/// Load `./textures/universal_grain.png` if present, otherwise bake the
/// procedural universal grain at `size × size`.
pub fn universal_grain_or_override(size: u32) -> Image {
    let path = format!("{TEX_DIR}/universal_grain.png");
    if let Some(img) = load_png_as_repeating_image(&path) {
        info!("textures: using override {path}");
        return img;
    }
    universal_grain(size)
}

/// Build the universal surface-grain image. Seamlessly tileable because
/// every noise source is sampled on a torus (sin/cos of uv).
pub fn universal_grain(size: u32) -> Image {
    let perlin_a = Perlin::new(1);
    let micro = Perlin::new(2);
    let warp_x = Perlin::new(3);
    let warp_y = Perlin::new(4);
    let strata = Perlin::new(5);
    let cell = Perlin::new(6);
    let cell2 = Perlin::new(7);
    let sparkle = Perlin::new(8);

    let mut data = Vec::with_capacity((size * size * 4) as usize);

    let s = size as f64;
    let two_pi = std::f64::consts::TAU;

    for y in 0..size {
        for x in 0..size {
            let u = x as f64 / s;
            let v = y as f64 / s;
            let tx = (u * two_pi).cos();
            let tz = (u * two_pi).sin();
            let ty = (v * two_pi).cos();
            let tw = (v * two_pi).sin();

            let wx = warp_x.get([tx * 1.7, ty * 1.7, tz * 1.7, tw * 1.7]) * 0.22;
            let wy = warp_y.get([tx * 1.7 + 5.1, ty * 1.7 - 3.4, tz * 1.7, tw * 1.7]) * 0.22;

            let mut n = 0.0;
            let mut amp = 1.0;
            let mut freq = 1.0;
            let mut norm = 0.0;
            for _ in 0..6 {
                n += amp
                    * perlin_a.get([
                        (tx + wx) * freq,
                        (ty + wy) * freq,
                        (tz + wx) * freq,
                        (tw + wy) * freq,
                    ]);
                norm += amp;
                amp *= 0.52;
                freq *= 2.0;
            }
            n /= norm.max(1e-6);

            let m = micro.get([tx * 9.0, ty * 9.0, tz * 9.0, tw * 9.0]) * 0.6
                + micro.get([tx * 21.0, ty * 21.0, tz * 21.0, tw * 21.0]) * 0.18;

            // Pseudo-Worley: two low-freq perlins, take min absolute value,
            // sharpen → bright ridge network that reads as cracks / mortar.
            let c1 = cell.get([tx * 3.2, ty * 3.2, tz * 3.2, tw * 3.2]).abs();
            let c2 = cell2
                .get([tx * 3.5 + 1.3, ty * 3.5, tz * 3.5, tw * 3.5])
                .abs();
            let ridge = (1.0 - c1.min(c2)).powf(8.0);
            let cracks = -ridge * 0.18;

            let st_raw = strata.get([tx * 0.6, ty * 4.5, tz * 0.6, tw * 4.5]);
            let st = (st_raw.abs() - 0.5) * 0.14;

            let sp_raw = sparkle.get([tx * 34.0, ty * 34.0, tz * 34.0, tw * 34.0]);
            let sp = if sp_raw > 0.74 {
                (sp_raw - 0.74) * 0.9
            } else {
                0.0
            };

            let brightness = 0.93 + n * 0.16 + m * 0.05 + st + cracks + sp;
            let brightness = brightness.clamp(0.68, 1.18);

            let b = (brightness.clamp(0.0, 1.0) * 255.0).round() as u8;
            data.push(b);
            data.push(b);
            data.push(b);
            data.push(255);
        }
    }

    make_repeating_image(size, size, data)
}

// ---------------------------------------------------------------------------
// Per-block swatches (for the Texture Viewer) -------------------------------
// ---------------------------------------------------------------------------

/// Small RGBA8 preview of one BlockType's material.
#[derive(Clone, Debug)]
pub struct BlockSwatch {
    pub block: BlockType,
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    /// Tightly-packed RGBA8 sRGB, row-major top-down.
    pub rgba: Vec<u8>,
}

impl BlockSwatch {
    pub fn to_png_bytes(&self) -> Option<Vec<u8>> {
        use image::ImageEncoder;
        let mut out: Vec<u8> = Vec::with_capacity(self.rgba.len() + 4096);
        {
            let mut cursor = std::io::Cursor::new(&mut out);
            image::codecs::png::PngEncoder::new(&mut cursor)
                .write_image(
                    &self.rgba,
                    self.width,
                    self.height,
                    image::ExtendedColorType::Rgba8,
                )
                .ok()?;
        }
        Some(out)
    }

    pub fn save_png(&self, dir: &str) -> std::io::Result<std::path::PathBuf> {
        let _ = std::fs::create_dir_all(dir);
        let path = std::path::Path::new(dir).join(format!("{}.png", self.name));
        let bytes = self
            .to_png_bytes()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "png encode failed"))?;
        std::fs::write(&path, bytes)?;
        Ok(path)
    }
}

/// Bake photorealistic preview swatches for every registered block type.
pub fn bake_all_block_swatches(size: u32) -> Vec<BlockSwatch> {
    use BlockType::*;
    let list: &[(BlockType, &str, BlockStyle)] = &[
        (Stone, "stone", BlockStyle::Rock),
        (Dirt, "dirt", BlockStyle::Soil),
        (Grass, "grass", BlockStyle::Grass),
        (Sand, "sand", BlockStyle::Sand),
        (Water, "water", BlockStyle::Water),
        (Wood, "wood", BlockStyle::Wood),
        (Leaves, "leaves", BlockStyle::Leaves),
        (Snow, "snow", BlockStyle::Snow),
        (Ice, "ice", BlockStyle::Ice),
        (TundraGrass, "tundra_grass", BlockStyle::Grass),
        (JungleLeaves, "jungle_leaves", BlockStyle::Leaves),
        (SavannaGrass, "savanna_grass", BlockStyle::Grass),
        (Gravel, "gravel", BlockStyle::Rock),
        (Bedrock, "bedrock", BlockStyle::Rock),
        (RedSand, "red_sand", BlockStyle::Sand),
        (RedStone, "red_stone", BlockStyle::Rock),
        (MesaClay, "mesa_clay", BlockStyle::Rock),
        (MossStone, "moss_stone", BlockStyle::Rock),
        (Limestone, "limestone", BlockStyle::Rock),
        (Crystal, "crystal", BlockStyle::Ice),
        (Basalt, "basalt", BlockStyle::Rock),
        (Lava, "lava", BlockStyle::Lava),
        (AlienMoss, "alien_moss", BlockStyle::Grass),
        (BoneRock, "bone_rock", BlockStyle::Rock),
        (GlowSand, "glow_sand", BlockStyle::Sand),
        (ShipHullDark, "ship_hull_dark", BlockStyle::Rock),
        (ShipHullAlloy, "ship_hull_alloy", BlockStyle::Rock),
        (CockpitGlass, "cockpit_glass", BlockStyle::Ice),
        (NeonCyan, "neon_cyan", BlockStyle::Ice),
        (NeonMagenta, "neon_magenta", BlockStyle::Ice),
        (NeonAmber, "neon_amber", BlockStyle::Lava),
        (EngineCore, "engine_core", BlockStyle::Lava),
        (LuminiteCrystal, "luminite_crystal", BlockStyle::Ice),
        (MagnetiteOre, "magnetite_ore", BlockStyle::Rock),
        (IridiumVein, "iridium_vein", BlockStyle::Ice),
        (BlossomLeaves, "blossom_leaves", BlockStyle::Leaves),
        (ZenStone, "zen_stone", BlockStyle::Rock),
        (Bamboo, "bamboo", BlockStyle::Wood),
        (SakuraPetals, "sakura_petals", BlockStyle::Leaves),
        (ShojiPaper, "shoji_paper", BlockStyle::Ice),
        (RoofTile, "roof_tile", BlockStyle::Rock),
        (TatamiMat, "tatami_mat", BlockStyle::Wood),
        (NeonGlass, "neon_glass", BlockStyle::Ice),
        (ShojiLamp, "shoji_lamp", BlockStyle::Lava),
    ];
    list.iter()
        .map(|(b, n, style)| bake_block_swatch(*b, n, *style, size))
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum BlockStyle {
    Rock,
    Soil,
    Grass,
    Sand,
    Water,
    Wood,
    Leaves,
    Snow,
    Ice,
    Lava,
}

fn bake_block_swatch(
    block: BlockType,
    name: &'static str,
    style: BlockStyle,
    size: u32,
) -> BlockSwatch {
    let color = block.color();
    let rgba = color.to_srgba();
    let base = [rgba.red, rgba.green, rgba.blue];
    // Water opacity belongs to its material/voxel tint, not its albedo map.
    // Keeping ripple texels opaque prevents accidental alpha multiplication
    // while retaining the existing procedural ripple colour resource.
    let alpha = if block == BlockType::Water {
        u8::MAX
    } else {
        (rgba.alpha.clamp(0.0, 1.0) * 255.0).round() as u8
    };

    let seed_base = block as u32;
    let perlin = Perlin::new(seed_base + 101);
    let detail = Perlin::new(seed_base + 202);
    let warp = Perlin::new(seed_base + 303);
    let strata = Perlin::new(seed_base + 404);
    let cell = Perlin::new(seed_base + 505);
    let macro_shape = Perlin::new(seed_base + 606);
    let grain = Perlin::new(seed_base + 707);
    let vein_noise = Perlin::new(seed_base + 808);

    let s = size as f64;
    let two_pi = std::f64::consts::TAU;
    let mut data = Vec::with_capacity((size * size * 4) as usize);

    for y in 0..size {
        for x in 0..size {
            let u = x as f64 / s;
            let v = y as f64 / s;
            let tx = (u * two_pi).cos();
            let tz = (u * two_pi).sin();
            let ty = (v * two_pi).cos();
            let tw = (v * two_pi).sin();

            let wx = warp.get([tx * 1.8, ty * 1.8, tz * 1.8, tw * 1.8]) * 0.2;
            let wy = warp.get([tx * 1.8 + 4.0, ty * 1.8 - 2.5, tz, tw]) * 0.2;

            let mut fbm = 0.0;
            let mut amp = 1.0;
            let mut freq = 1.0;
            let mut norm = 0.0;
            for _ in 0..5 {
                fbm += amp
                    * perlin.get([
                        (tx + wx) * freq,
                        (ty + wy) * freq,
                        (tz + wx) * freq,
                        (tw + wy) * freq,
                    ]);
                norm += amp;
                amp *= 0.55;
                freq *= 2.0;
            }
            fbm /= norm.max(1e-6);

            let micro = detail.get([tx * 14.0, ty * 14.0, tz * 14.0, tw * 14.0]) * 0.35;
            let strat = strata.get([tx * 0.7, ty * 5.0, tz * 0.7, tw * 5.0]);
            let cell_n = (1.0 - cell.get([tx * 4.0, ty * 4.0, tz * 4.0, tw * 4.0]).abs()).powf(7.0);
            let macro_n = macro_shape.get([tx * 0.55, ty * 0.55, tz * 0.55, tw * 0.55]);
            let broad = macro_shape.get([tx * 1.15 + 9.0, ty * 1.15 - 2.0, tz * 1.15, tw * 1.15]);
            let grain_n = grain.get([tx * 38.0, ty * 38.0, tz * 38.0, tw * 38.0]);
            let brushed = grain.get([tx * 5.2 + fbm, ty * 1.4, tz * 5.2, tw * 1.4]);
            let vein = (1.0
                - vein_noise
                    .get([tx * 2.7 + 1.7, ty * 2.7, tz * 2.7 - 3.1, tw * 2.7])
                    .abs())
            .powf(9.0);
            let vein_wide = (1.0
                - vein_noise
                    .get([tx * 1.45 - 4.0, ty * 1.45 + 2.0, tz * 1.45, tw * 1.45])
                    .abs())
            .powf(4.0);
            let macro_shadow = macro_n * 0.06 + broad * 0.04;

            let (bright, tint_r, tint_g, tint_b) = match style {
                BlockStyle::Rock => match block {
                    // Pale karst occupies enormous contiguous faces. Generic
                    // aggregate/vein contrast becomes high-frequency crawling
                    // noise at play distance, so limestone keeps only broad
                    // sediment and a restrained porous grain.
                    BlockType::Limestone => {
                        let sediment = (strat * 0.55 + broad * 0.45) * 0.045;
                        let pore = (grain_n.abs() - 0.58).max(0.0) * 0.055;
                        let b = 0.93 + macro_shadow * 0.42 + fbm * 0.10 + micro * 0.035 + sediment
                            - cell_n * 0.055
                            - vein * 0.045
                            - pore;
                        let warm = (broad * 0.015 + sediment * 0.08) as f32;
                        (b, warm, warm * 0.45, -warm * 0.55)
                    }
                    // Mossy ledges need readable colony colour, not the same
                    // sharp cracks as exposed stone. Macro lichen remains, but
                    // the pixel-scale variation is deliberately quieter.
                    BlockType::MossStone => {
                        let lichen = macro_n.max(0.0) * 0.075 + broad.max(0.0) * 0.035;
                        let b = 0.87 + macro_shadow * 0.58 + fbm * 0.12 + micro * 0.050 + lichen
                            - cell_n * 0.075
                            - vein * 0.055;
                        (
                            b,
                            -(lichen as f32) * 0.030,
                            (lichen as f32) * 0.085,
                            -(lichen as f32) * 0.045,
                        )
                    }
                    _ => {
                        let aggregate = (grain_n.abs() - 0.35).max(0.0) * 0.32;
                        let slab = (u * two_pi * 3.0 + v * two_pi * 2.0 + broad * 2.5).sin() * 0.12;
                        let sediment =
                            (u * two_pi * 1.35 - v * two_pi * 3.10 + macro_n * 3.4).sin() * 0.10;
                        let weathered_face = if vein_wide > 0.42 {
                            (vein_wide - 0.42) * 0.28
                        } else {
                            0.0
                        };
                        let mineral_wash = (macro_n * 0.045 + broad * 0.03) as f32;
                        let b = 0.86 + macro_shadow * 1.25 + fbm * 0.28 + micro * 0.18
                            - cell_n * 0.34
                            - vein * 0.28
                            + (strat.abs() - 0.5) * 0.18
                            + aggregate
                            + slab
                            + sediment
                            - weathered_face;
                        let d = (fbm * 0.045 + broad * 0.025) as f32;
                        (
                            b,
                            d + mineral_wash + (sediment as f32) * 0.035,
                            (vein_wide as f32) * 0.030 - mineral_wash * 0.25,
                            -d * 0.7 - mineral_wash * 0.45 - (weathered_face as f32) * 0.035,
                        )
                    }
                },
                BlockStyle::Soil => {
                    let pore = (grain_n.abs() - 0.20).max(0.0) * 0.10;
                    let b = 0.82 + macro_shadow * 1.2 + fbm * 0.22 + micro * 0.19 - cell_n * 0.10
                        + pore;
                    (
                        b,
                        (micro as f32) * 0.07 + (broad as f32) * 0.025,
                        -(micro as f32) * 0.035,
                        -(cell_n as f32) * 0.015,
                    )
                }
                BlockStyle::Grass => {
                    // Grass must work at two incompatible viewing scales:
                    // coherent colour masses from a shuttle and crisp fibres
                    // at walking height. The former broad sine waves were so
                    // strong that a repeated swatch looked like blurred green
                    // camouflage. These narrow periodic ridges behave like a
                    // tiny anisotropic normal/albedo cue while quiet FBM owns
                    // the larger meadow tone. Every term is periodic, so the
                    // swatch remains exactly seamless.
                    let fibre_a =
                        ((u * two_pi * 23.0 + v * two_pi * 5.0 + broad * 1.7).sin() * 0.5 + 0.5)
                            .powf(7.0);
                    let fibre_b =
                        ((u * two_pi * 11.0 - v * two_pi * 19.0 + macro_n * 1.9).sin() * 0.5 + 0.5)
                            .powf(9.0);
                    let fibre_gutter =
                        (1.0 - (u * two_pi * 17.0 + v * two_pi * 7.0 + fbm).sin().abs()).powf(10.0);
                    let blade_noise = detail.get([tx * 22.0, ty * 8.0, tz * 22.0, tw * 8.0]);
                    let moss_patch = macro_n.max(0.0) * 0.08;
                    let soil_fleck = if grain_n < -0.72 { 0.09 } else { 0.0 };
                    let lush_patch = broad.max(0.0) * 0.09;
                    let dry_patch = if vein_wide > 0.62 {
                        (vein_wide - 0.62) * 0.24
                    } else {
                        0.0
                    };
                    let b = 0.88
                        + macro_shadow * 0.62
                        + fbm * 0.09
                        + blade_noise * 0.035
                        + micro * 0.025
                        + fibre_a * 0.075
                        + fibre_b * 0.050
                        - fibre_gutter * 0.055
                        + moss_patch
                        + lush_patch * 0.34
                        - soil_fleck * 0.72
                        - dry_patch * 0.70;
                    (
                        b,
                        (dry_patch as f32) * 0.08 - (soil_fleck as f32) * 0.03,
                        (fibre_a as f32) * 0.026
                            + (fibre_b as f32) * 0.018
                            + (moss_patch as f32) * 0.08
                            + (lush_patch as f32) * 0.10
                            - (dry_patch as f32) * 0.04,
                        (lush_patch as f32) * 0.035
                            - (soil_fleck as f32) * 0.03
                            - (dry_patch as f32) * 0.06,
                    )
                }
                BlockStyle::Sand => {
                    let ripple_a = (v * two_pi * 8.0 + broad * 2.6 + fbm * 2.4).sin() * 0.055;
                    let ripple_b = ((u + v * 0.42) * two_pi * 4.5 + macro_n * 2.8).sin() * 0.025;
                    let dune_shadow = broad.min(0.0).abs() * 0.035;
                    let mineral = (grain_n - 0.50).max(0.0) * 0.10;
                    let quartz = if vein > 0.46 {
                        (vein - 0.46) * 0.35
                    } else {
                        0.0
                    };
                    (
                        0.86 + macro_shadow * 0.45
                            + fbm * 0.08
                            + micro * 0.11
                            + ripple_a
                            + ripple_b
                            - dune_shadow
                            + mineral
                            + quartz,
                        mineral as f32 * 0.035 + quartz as f32 * 0.020,
                        -(dune_shadow as f32) * 0.020,
                        -(mineral as f32) * 0.050 - (dune_shadow as f32) * 0.035,
                    )
                }
                BlockStyle::Water => {
                    let ripple_a = (u * two_pi * 4.0 + broad * 2.0 + fbm * 3.0).sin() * 0.05;
                    let ripple_b = (v * two_pi * 3.0 + macro_n * 2.0 + fbm * 2.0).cos() * 0.05;
                    let caustic = vein_wide * 0.07;
                    (
                        1.0 + ripple_a + ripple_b + micro * 0.05 + caustic,
                        0.0,
                        caustic as f32 * 0.04,
                        0.04,
                    )
                }
                BlockStyle::Wood => {
                    // Longitudinal bark plates. Earlier radial rings appeared
                    // on every trunk side and read like repeated red targets.
                    // The mesher keeps world Y on the texture's V axis for
                    // side faces, so this restrained pattern rises with the
                    // tree while still rotating safely around its trunk.
                    let plate = (u * two_pi * 3.0 + fbm * 1.35 + broad * 0.65).sin();
                    let fine_plate = (u * two_pi * 9.0 + macro_n * 1.8).sin();
                    let fissure_noise = detail.get([tx * 9.0, ty * 2.0, tz * 9.0, tw * 2.0]);
                    let fissure = (-fissure_noise - 0.42).max(0.0);
                    let vertical_weather = (v * two_pi * 0.65 + broad).sin();
                    let b = 0.88
                        + macro_shadow * 0.42
                        + plate * 0.085
                        + fine_plate * 0.035
                        + vertical_weather * 0.025
                        + brushed * 0.045
                        + micro * 0.045
                        - fissure * 0.15;
                    (
                        b,
                        (plate as f32) * 0.018,
                        -(fissure as f32) * 0.020,
                        -(plate as f32) * 0.010,
                    )
                }
                BlockStyle::Leaves => {
                    let canopy = cell_n * 0.26 + macro_n.max(0.0) * 0.10;
                    let twig = if grain_n < -0.62 { 0.10 } else { 0.0 };
                    let b = 0.76 + macro_shadow + canopy + fbm * 0.17 + micro * 0.11 - twig * 0.75;
                    (
                        b,
                        -(twig as f32) * 0.035,
                        (canopy as f32) * 0.08,
                        -(twig as f32) * 0.025,
                    )
                }
                BlockStyle::Snow => {
                    let sparkle = detail.get([tx * 40.0, ty * 40.0, tz * 40.0, tw * 40.0]);
                    let sp = if sparkle > 0.72 {
                        (sparkle - 0.72) * 1.8
                    } else {
                        0.0
                    };
                    (
                        0.96 + macro_shadow * 0.45 + fbm * 0.05 + micro * 0.03 + sp,
                        0.0,
                        0.0,
                        0.0,
                    )
                }
                BlockStyle::Ice => {
                    let facet = perlin.get([tx * 3.0, ty * 3.0, tz * 3.0, tw * 3.0]);
                    let sparkle = detail.get([tx * 32.0, ty * 32.0, tz * 32.0, tw * 32.0]);
                    let sp = if sparkle > 0.78 {
                        (sparkle - 0.78) * 2.0
                    } else {
                        0.0
                    };
                    let crystalline = vein_wide * 0.08;
                    (
                        0.92 + macro_shadow * 0.6 + facet * 0.10 + crystalline + sp,
                        0.0,
                        crystalline as f32 * 0.025,
                        0.045,
                    )
                }
                BlockStyle::Lava => {
                    let heat = cell_n.max(vein_wide);
                    let b = 0.62 + macro_n.abs() * 0.08 + heat * 0.54 + fbm * 0.18;
                    (
                        b,
                        (heat as f32) * 0.28,
                        (heat as f32) * 0.11 + (vein as f32) * 0.04,
                        0.0,
                    )
                }
            };

            let bright = bright.clamp(0.55, 1.30) as f32;
            let r = (base[0] * bright + tint_r).clamp(0.0, 1.0);
            let g = (base[1] * bright + tint_g).clamp(0.0, 1.0);
            let bl = (base[2] * bright + tint_b).clamp(0.0, 1.0);

            data.push((r * 255.0).round() as u8);
            data.push((g * 255.0).round() as u8);
            data.push((bl * 255.0).round() as u8);
            let texel_alpha = if matches!(style, BlockStyle::Leaves) {
                // Near canopies gain small, irregular sky holes instead of
                // reading as sealed cubes. Lower mip levels average the mask,
                // naturally restoring a solid silhouette at flight distance.
                let pore_signal = grain_n * 0.78 + micro * 0.22;
                if pore_signal < -0.26 && cell_n < 0.48 {
                    0
                } else {
                    alpha
                }
            } else {
                alpha
            };
            data.push(texel_alpha);
        }
    }

    BlockSwatch {
        block,
        name,
        width: size,
        height: size,
        rgba: data,
    }
}

// ---------------------------------------------------------------------------
// Helpers -------------------------------------------------------------------
// ---------------------------------------------------------------------------

fn make_repeating_image(w: u32, h: u32, data: Vec<u8>) -> Image {
    let (data, mip_level_count) = rgba8_srgb_mip_chain(w, h, data);
    Image {
        // Construct directly: `Image::new` accepts one base level and formerly
        // forced a full base-level clone before this complete chain replaced it.
        data,
        texture_descriptor: TextureDescriptor {
            label: None,
            size: Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        },
        sampler: ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            address_mode_w: ImageAddressMode::Repeat,
            anisotropy_clamp: 8,
            ..ImageSamplerDescriptor::linear()
        }),
        texture_view_descriptor: None,
        asset_usage: RenderAssetUsages::default(),
    }
}

/// Build a complete, tightly packed mip chain for an sRGB RGBA8 image.
///
/// RGB is averaged in linear light, then encoded back to sRGB. Averaging the
/// bytes directly would darken thin sand, bark and foliage detail. Alpha stays
/// linear. The smallest level is always 1x1, including non-power-of-two custom
/// textures.
fn rgba8_srgb_mip_chain(mut width: u32, mut height: u32, base: Vec<u8>) -> (Vec<u8>, u32) {
    let expected = width as usize * height as usize * 4;
    if width == 0 || height == 0 || base.len() != expected {
        return (base, 1);
    }

    let srgb_to_linear: [f32; 256] = std::array::from_fn(|value| {
        let encoded = value as f32 / 255.0;
        if encoded <= 0.040_45 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    });
    let encode_srgb = |linear: f32| -> u8 {
        let linear = linear.clamp(0.0, 1.0);
        let encoded = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (encoded * 255.0).round().clamp(0.0, 255.0) as u8
    };

    // Move the decoder's base Vec and append every lower level directly into
    // it. There is no base clone, no second full chain, and no per-level Vec:
    // explicit pixel-buffer lengths never exceed the final mip payload.
    let exact_payload = rgba8_mip_payload_bytes(width, height).unwrap_or(expected);
    let mut chain = base;
    chain.reserve_exact(exact_payload.saturating_sub(chain.len()));
    let mut current_offset = 0usize;
    let mut levels = 1;

    while width > 1 || height > 1 {
        let next_width = (width / 2).max(1);
        let next_height = (height / 2).max(1);

        for y in 0..next_height {
            for x in 0..next_width {
                // Integer partitions include every source texel even for
                // odd/non-power-of-two custom images (for example 5 -> 2
                // consumes 0..2 and 2..5 rather than dropping the last row).
                let source_x_start = x * width / next_width;
                let source_x_end = ((x + 1) * width / next_width).max(source_x_start + 1);
                let source_y_start = y * height / next_height;
                let source_y_end = ((y + 1) * height / next_height).max(source_y_start + 1);
                let mut linear_rgb = [0.0_f32; 3];
                let mut alpha = 0u32;
                let mut samples = 0u32;
                for sy in source_y_start..source_y_end {
                    for sx in source_x_start..source_x_end {
                        let source = current_offset + ((sy * width + sx) * 4) as usize;
                        for channel in 0..3 {
                            linear_rgb[channel] += srgb_to_linear[chain[source + channel] as usize];
                        }
                        alpha += u32::from(chain[source + 3]);
                        samples += 1;
                    }
                }

                for channel in 0..3 {
                    chain.push(encode_srgb(linear_rgb[channel] / samples as f32));
                }
                chain.push(((alpha + samples / 2) / samples) as u8);
            }
        }

        current_offset = current_offset.saturating_add(width as usize * height as usize * 4);
        width = next_width;
        height = next_height;
        levels += 1;
    }

    debug_assert_eq!(chain.len(), exact_payload);
    (chain, levels)
}

fn load_png_as_repeating_image(path: &str) -> Option<Image> {
    let source = open_custom_png_once(Path::new(path)).ok()?;
    load_bounded_custom_png(source, MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SWATCH_SIZE: u32 = 4;
    const TEST_A_ID: MaterialId = DURABLE_CUSTOM_MATERIAL_BASE;
    const TEST_B_ID: MaterialId = DURABLE_CUSTOM_MATERIAL_BASE + 1;
    const TEST_C_ID: MaterialId = DURABLE_CUSTOM_MATERIAL_BASE + 2;
    const TEST_SAFE_ID: MaterialId = DURABLE_CUSTOM_MATERIAL_BASE + 3;

    fn rebuild_without_custom(
        library: &mut MaterialLibrary,
        materials: &mut Assets<StandardMaterial>,
        vegetation_materials: &mut Assets<crate::vegetation::VegetationMaterial>,
        images: &mut Assets<Image>,
    ) {
        library.rebuild_without_custom_for_test(
            materials,
            vegetation_materials,
            images,
            TEST_SWATCH_SIZE,
        );
    }

    fn prepared_custom(id: MaterialId, name: &str, rgba: [u8; 4]) -> PreparedCustomMaterial {
        prepared_custom_sized(id, name, 1, rgba)
    }

    fn prepared_custom_sized(
        id: MaterialId,
        name: &str,
        size: u32,
        rgba: [u8; 4],
    ) -> PreparedCustomMaterial {
        PreparedCustomMaterial {
            id,
            source_key: durable_custom_source_key(id),
            name: name.to_string(),
            image: make_repeating_image(size, size, rgba.repeat(size as usize * size as usize)),
        }
    }

    fn custom_payloads(
        library: &MaterialLibrary,
        materials: &Assets<StandardMaterial>,
        images: &Assets<Image>,
    ) -> (usize, usize) {
        let active: BTreeSet<_> = library.custom_ids.iter().copied().collect();
        let mut active_payload = 0usize;
        let mut tombstone_payload = 0usize;
        for id in library.custom_sources.keys() {
            let material = materials
                .get(library.handles.get(id).unwrap())
                .expect("registered custom material");
            let image = images
                .get(material.base_color_texture.as_ref().unwrap())
                .expect("registered custom image");
            if active.contains(id) {
                active_payload += image.data.len();
            } else {
                tombstone_payload += image.data.len();
            }
        }
        (active_payload, tombstone_payload)
    }

    #[test]
    fn custom_image_payload_ceilings_are_exact_and_include_old_plus_candidate() {
        assert_eq!(
            rgba8_mip_payload_bytes(MAX_CUSTOM_PNG_EDGE_PIXELS, MAX_CUSTOM_PNG_EDGE_PIXELS),
            Some(89_478_484)
        );
        assert_eq!(MAX_CUSTOM_ACTIVE_IMAGE_PAYLOAD_BYTES, 268_435_456);
        assert_eq!(CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES, 4);
        assert_eq!(UNRESOLVED_CUSTOM_IMAGE_PAYLOAD_BYTES, 4);
        assert_eq!(MAX_CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES, 16_384);
        assert_eq!(MAX_CUSTOM_RESIDENT_IMAGE_PAYLOAD_BYTES, 268_451_844);
        assert_eq!(
            MAX_CUSTOM_RELOAD_TRANSIENT_IMAGE_PAYLOAD_BYTES, 536_887_300,
            "transient Image payload must include old resident plus staged candidate"
        );
        assert_eq!(MAX_CUSTOM_PNG_SOURCE_BYTES, 67_108_864);
        assert_eq!(MAX_CUSTOM_TOTAL_PNG_SOURCE_BYTES, 268_435_456);
        assert_eq!(MAX_CUSTOM_PNG_DECODER_ALLOCATION_BYTES, 268_435_456);
        assert_eq!(MAX_CUSTOM_RELOAD_DECLARED_CPU_BYTES, 872_431_620);
        assert_eq!(MAX_CUSTOM_MATERIAL_DIRECTORY_ENTRIES, 8_192);
    }

    #[test]
    fn directory_scan_cap_counts_non_png_entries_before_filtering() {
        let mut scanned = 0usize;
        // The production loop calls this before checking an extension; these
        // represent unrelated/non-PNG entries just as much as PNGs.
        for _ in 0..MAX_CUSTOM_MATERIAL_DIRECTORY_ENTRIES {
            count_custom_directory_entry(&mut scanned).unwrap();
        }
        assert_eq!(scanned, MAX_CUSTOM_MATERIAL_DIRECTORY_ENTRIES);
        let error = count_custom_directory_entry(&mut scanned).unwrap_err();
        assert!(error.contains("mehr als 8192"));
    }

    #[test]
    fn total_compressed_source_work_has_an_exact_256_mib_cap() {
        let mut total = 0u64;
        for _ in 0..4 {
            total = checked_custom_source_total(total, MAX_CUSTOM_PNG_SOURCE_BYTES).unwrap();
        }
        assert_eq!(total, MAX_CUSTOM_TOTAL_PNG_SOURCE_BYTES);
        assert!(checked_custom_source_total(total, 1).is_err());
        assert!(checked_custom_source_total(u64::MAX, 1).is_err());
    }

    #[test]
    fn bounded_reader_accepts_exact_limit_and_rejects_limit_plus_one_growth() {
        let mut exact = std::io::Cursor::new(vec![7u8; 8]);
        assert_eq!(read_bounded(&mut exact, 8, 1).unwrap(), vec![7u8; 8]);

        // Capacity says four bytes (like stale open metadata), while the
        // reader produces MAX+1. `take(MAX+1)` observes exactly the sentinel
        // byte and fails without reading or allocating beyond that bound.
        let mut grew_after_metadata = std::io::Cursor::new(vec![9u8; 9]);
        let error = read_bounded(&mut grew_after_metadata, 8, 4).unwrap_err();
        assert!(error.contains("Leselimit"));
        assert_eq!(grew_after_metadata.position(), 9);
    }

    #[test]
    fn malformed_bounded_png_fails_before_any_image_is_published() {
        let mut source = std::io::Cursor::new(b"not a png".to_vec());
        let bounded = read_bounded(&mut source, 64, 0).unwrap();
        let error = decode_custom_png(bounded, 1_024).unwrap_err();
        assert!(error.contains("Header"));
    }

    #[test]
    fn one_open_validator_rejects_non_regular_sources() {
        let error = open_custom_png_once(Path::new(".")).unwrap_err();
        assert!(error.contains("regulaere Datei"));
    }

    #[test]
    fn one_open_validator_accepts_a_stable_regular_file_handle() {
        let source = open_custom_png_once(Path::new("Cargo.toml")).unwrap();
        assert!(source.opened_len > 0);
        assert_eq!(source.file.metadata().unwrap().len(), source.opened_len);
    }

    #[test]
    fn explicit_custom_slot_parsing_is_order_independent_and_collision_detecting() {
        let (id, key, name) =
            parse_durable_custom_filename("material-32768__violet-rock.png").unwrap();
        assert_eq!(id, DURABLE_CUSTOM_MATERIAL_BASE);
        assert_eq!(key, "material-id:32768");
        assert_eq!(name, "violet-rock");
        assert_eq!(
            parse_durable_custom_filename("material-65535__last-slot.PNG")
                .unwrap()
                .0,
            u16::MAX
        );
        for ambiguous in [
            "legacy-list-name.png",
            "material-01024__legacy.png",
            "material-32768__.png",
            "material-65536__overflow.png",
        ] {
            assert!(
                parse_durable_custom_filename(ambiguous).is_err(),
                "{ambiguous} must require explicit safe adoption"
            );
        }

        let plan = plan_custom_materials(
            vec![
                prepared_custom(TEST_B_ID, "b", [1, 2, 3, 255]),
                prepared_custom(TEST_A_ID, "a", [4, 5, 6, 255]),
            ],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            plan.materials
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![TEST_A_ID, TEST_B_ID]
        );
        let collision = plan_custom_materials(
            vec![
                prepared_custom(TEST_A_ID, "first", [1, 1, 1, 255]),
                prepared_custom(TEST_A_ID, "second", [2, 2, 2, 255]),
            ],
            &BTreeMap::new(),
        )
        .err()
        .expect("duplicate slot must fail");
        assert!(collision.contains("doppelte dauerhafte Custom-MaterialId"));
    }

    #[test]
    fn process_registry_rejects_ambiguous_legacy_assignment() {
        let legacy = BTreeMap::from([(CUSTOM_MATERIAL_BASE, "legacy-list-name.png".to_string())]);
        let error = plan_custom_materials(Vec::new(), &legacy)
            .err()
            .expect("legacy mapping must fail");
        assert!(error.contains("mehrdeutiger historischer Custom-Eintrag"));
    }

    #[test]
    fn rebuild_updates_builtin_standard_and_vegetation_assets_in_place() {
        let mut library = MaterialLibrary::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let mut vegetation_materials = Assets::<crate::vegetation::VegetationMaterial>::default();
        let mut images = Assets::<Image>::default();
        rebuild_without_custom(
            &mut library,
            &mut materials,
            &mut vegetation_materials,
            &mut images,
        );

        let stone_id = BlockType::Stone as MaterialId;
        let leaves_id = BlockType::Leaves as MaterialId;
        let old_standard = library.handles.get(&stone_id).unwrap().clone();
        let old_vegetation = library.vegetation_handles.get(&leaves_id).unwrap().clone();
        let old_image = materials
            .get(&old_standard)
            .unwrap()
            .base_color_texture
            .clone()
            .unwrap();
        let old_vegetation_image = vegetation_materials
            .get(&old_vegetation)
            .unwrap()
            .base
            .base_color_texture
            .clone()
            .unwrap();
        let original_image_byte = images.get(&old_image).unwrap().data[0];

        materials
            .get_mut(&old_standard)
            .unwrap()
            .perceptual_roughness = 0.01;
        let vegetation = vegetation_materials.get_mut(&old_vegetation).unwrap();
        vegetation.base.perceptual_roughness = 0.02;
        vegetation.extension.parameters.direction_macro.z = 0.0;
        images.get_mut(&old_image).unwrap().data[0] ^= 0xff;

        rebuild_without_custom(
            &mut library,
            &mut materials,
            &mut vegetation_materials,
            &mut images,
        );

        assert_eq!(
            library.handles.get(&stone_id).unwrap().id(),
            old_standard.id()
        );
        assert_eq!(
            library.vegetation_handles.get(&leaves_id).unwrap().id(),
            old_vegetation.id()
        );
        let stone = materials
            .get(&old_standard)
            .expect("old standard handle sees replacement");
        assert_eq!(
            stone.perceptual_roughness,
            terrain_material_profile(BlockType::Stone).perceptual_roughness
        );
        assert_eq!(
            stone.base_color_texture.as_ref().unwrap().id(),
            old_image.id()
        );
        assert_eq!(images.get(&old_image).unwrap().data[0], original_image_byte);

        let leaves = vegetation_materials
            .get(&old_vegetation)
            .expect("old vegetation handle sees replacement");
        let expected = crate::vegetation::VegetationWind::for_block(BlockType::Leaves)
            .unwrap()
            .parameters;
        assert_eq!(
            leaves.base.perceptual_roughness,
            terrain_material_profile(BlockType::Leaves).perceptual_roughness
        );
        assert_eq!(
            leaves.extension.parameters.direction_macro,
            expected.direction_macro
        );
        assert_eq!(
            leaves.extension.parameters.flutter_phase,
            expected.flutter_phase
        );
        assert_eq!(
            leaves.base.base_color_texture.as_ref().unwrap().id(),
            old_vegetation_image.id()
        );
    }

    #[test]
    fn repeated_builtin_rebuild_has_constant_asset_population() {
        let mut library = MaterialLibrary::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let mut vegetation_materials = Assets::<crate::vegetation::VegetationMaterial>::default();
        let mut images = Assets::<Image>::default();
        rebuild_without_custom(
            &mut library,
            &mut materials,
            &mut vegetation_materials,
            &mut images,
        );
        let expected = (materials.len(), vegetation_materials.len(), images.len());
        assert_eq!(expected, (45, 4, 45));
        assert_eq!(expected.0, library.handles.len() + 1);
        assert_eq!(expected.1, library.vegetation_handles.len());
        for species in VegetationSpecies::ALL {
            let canonical = library
                .vegetation_handles
                .get(&(species.block() as MaterialId))
                .expect("canonical foliage material");
            let routed = library
                .vegetation_handle_for_species(species)
                .expect("species route");
            assert_eq!(routed.id(), canonical.id());
            let material = vegetation_materials.get(&routed).unwrap();
            let authored = crate::vegetation::VegetationWind::for_block(species.block())
                .expect("authoritative species preset");
            assert_eq!(
                material.extension.parameters.direction_macro,
                authored.parameters.direction_macro
            );
            assert_eq!(
                material.extension.parameters.flutter_phase,
                authored.parameters.flutter_phase
            );
        }

        for _ in 0..5 {
            rebuild_without_custom(
                &mut library,
                &mut materials,
                &mut vegetation_materials,
                &mut images,
            );
            assert_eq!(
                (materials.len(), vegetation_materials.len(), images.len()),
                expected
            );
        }
    }

    #[test]
    fn unresolved_custom_ids_use_one_stable_magenta_asset_not_stone() {
        let mut library = MaterialLibrary::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let mut vegetation_materials = Assets::<crate::vegetation::VegetationMaterial>::default();
        let mut images = Assets::<Image>::default();
        rebuild_without_custom(
            &mut library,
            &mut materials,
            &mut vegetation_materials,
            &mut images,
        );

        let unresolved_material = library
            .handle_for(CUSTOM_MATERIAL_BASE)
            .expect("raw custom id must remain visibly unresolved");
        let stone = library
            .handles
            .get(&(BlockType::Stone as MaterialId))
            .unwrap();
        assert_ne!(unresolved_material.id(), stone.id());
        assert_eq!(
            unresolved_material.id(),
            library.unresolved_custom_handle.as_ref().unwrap().id()
        );
        let unresolved_image = materials
            .get(&unresolved_material)
            .unwrap()
            .base_color_texture
            .clone()
            .unwrap();
        assert_eq!(
            images.get(&unresolved_image).unwrap().data,
            CUSTOM_TOMBSTONE_RGBA
        );
        let population = (materials.len(), vegetation_materials.len(), images.len());

        rebuild_without_custom(
            &mut library,
            &mut materials,
            &mut vegetation_materials,
            &mut images,
        );
        assert_eq!(
            library.handle_for(u16::MAX).unwrap().id(),
            unresolved_material.id()
        );
        let reloaded_image = materials
            .get(&unresolved_material)
            .unwrap()
            .base_color_texture
            .as_ref()
            .unwrap();
        assert_eq!(reloaded_image.id(), unresolved_image.id());
        assert_eq!(
            images.get(reloaded_image).unwrap().data,
            CUSTOM_TOMBSTONE_RGBA
        );
        assert_eq!(
            (materials.len(), vegetation_materials.len(), images.len()),
            population
        );
        assert!(library.status.contains("unbekannt/legacy -> Magenta"));
        assert!(library.status.contains("nicht beweisbar"));
    }

    #[test]
    fn custom_sources_keep_identity_across_reorder_removal_and_readdition() {
        let mut library = MaterialLibrary::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let mut vegetation_materials = Assets::<crate::vegetation::VegetationMaterial>::default();
        let mut images = Assets::<Image>::default();
        library.rebuild_prepared(
            &mut materials,
            &mut vegetation_materials,
            &mut images,
            TEST_SWATCH_SIZE,
            Ok(vec![
                prepared_custom(TEST_B_ID, "b", [0, 20, 0, 255]),
                prepared_custom_sized(TEST_A_ID, "a", 4, [10, 0, 0, 255]),
            ]),
        );
        let a_id = TEST_A_ID;
        let b_id = TEST_B_ID;
        let a_handle = library.handles.get(&a_id).unwrap().clone();
        let b_handle = library.handles.get(&b_id).unwrap().clone();
        let a_image_handle = materials
            .get(&a_handle)
            .unwrap()
            .base_color_texture
            .clone()
            .unwrap();
        let population = (materials.len(), vegetation_materials.len(), images.len());

        library.rebuild_prepared(
            &mut materials,
            &mut vegetation_materials,
            &mut images,
            TEST_SWATCH_SIZE,
            Ok(vec![
                prepared_custom_sized(TEST_A_ID, "a", 4, [30, 0, 0, 255]),
                prepared_custom(TEST_B_ID, "b", [0, 40, 0, 255]),
            ]),
        );
        assert_eq!(
            library.custom_sources.get(&a_id),
            Some(&durable_custom_source_key(a_id))
        );
        assert_eq!(
            library.custom_sources.get(&b_id),
            Some(&durable_custom_source_key(b_id))
        );
        assert_eq!(library.handles.get(&a_id).unwrap().id(), a_handle.id());
        assert_eq!(library.handles.get(&b_id).unwrap().id(), b_handle.id());
        let a_image = materials
            .get(&a_handle)
            .unwrap()
            .base_color_texture
            .as_ref()
            .unwrap();
        let b_image = materials
            .get(&b_handle)
            .unwrap()
            .base_color_texture
            .as_ref()
            .unwrap();
        assert_eq!(a_image.id(), a_image_handle.id());
        assert_eq!(&images.get(a_image).unwrap().data[..4], &[30, 0, 0, 255]);
        assert_eq!(&images.get(b_image).unwrap().data[..4], &[0, 40, 0, 255]);
        assert_eq!(
            (materials.len(), vegetation_materials.len(), images.len()),
            population
        );

        library.rebuild_prepared(
            &mut materials,
            &mut vegetation_materials,
            &mut images,
            TEST_SWATCH_SIZE,
            Ok(vec![
                prepared_custom(TEST_B_ID, "b", [0, 50, 0, 255]),
                prepared_custom(TEST_C_ID, "c", [0, 0, 60, 255]),
            ]),
        );
        let c_id = TEST_C_ID;
        assert_ne!(c_id, a_id, "a retired id must not be reassigned to c");
        assert_eq!(
            library.custom_sources.get(&b_id),
            Some(&durable_custom_source_key(b_id))
        );
        assert_eq!(library.handles.get(&a_id).unwrap().id(), a_handle.id());
        let retained_a_image = materials
            .get(&a_handle)
            .unwrap()
            .base_color_texture
            .as_ref()
            .unwrap();
        assert_eq!(retained_a_image.id(), a_image_handle.id());
        assert_eq!(
            images.get(retained_a_image).unwrap().data,
            CUSTOM_TOMBSTONE_RGBA
        );
        assert_eq!(library.custom_tombstone_image_payload_bytes, 4);
        assert!(!library.custom_ids.contains(&a_id));
        assert!(library.custom_ids.contains(&b_id));
        assert!(library.custom_ids.contains(&c_id));

        let changed_population = (materials.len(), vegetation_materials.len(), images.len());
        library.rebuild_prepared(
            &mut materials,
            &mut vegetation_materials,
            &mut images,
            TEST_SWATCH_SIZE,
            Ok(vec![
                prepared_custom(TEST_C_ID, "c", [0, 0, 70, 255]),
                prepared_custom(TEST_B_ID, "b", [0, 80, 0, 255]),
            ]),
        );
        assert_eq!(
            (materials.len(), vegetation_materials.len(), images.len()),
            changed_population
        );

        library.rebuild_prepared(
            &mut materials,
            &mut vegetation_materials,
            &mut images,
            TEST_SWATCH_SIZE,
            Ok(vec![
                prepared_custom_sized(TEST_A_ID, "a", 4, [90, 0, 0, 255]),
                prepared_custom(TEST_C_ID, "c", [0, 0, 100, 255]),
            ]),
        );
        assert_eq!(
            library.custom_sources.get(&a_id),
            Some(&durable_custom_source_key(a_id))
        );
        assert_eq!(library.handles.get(&a_id).unwrap().id(), a_handle.id());
        let restored_a_image = materials
            .get(&a_handle)
            .unwrap()
            .base_color_texture
            .as_ref()
            .unwrap();
        assert_eq!(restored_a_image.id(), a_image_handle.id());
        assert_eq!(
            &images.get(restored_a_image).unwrap().data[..4],
            &[90, 0, 0, 255]
        );
        assert_eq!(
            (materials.len(), vegetation_materials.len(), images.len()),
            changed_population,
            "re-adding a known source must reuse its retired assets"
        );
    }

    #[test]
    fn custom_churn_to_identity_cap_keeps_only_active_payload_plus_four_byte_tombstones() {
        const BATCH_SIZE: usize = 64;
        const ROUNDS: usize = MAX_CUSTOM_MATERIAL_IDENTITIES / BATCH_SIZE;
        const ACTIVE_IMAGE_SIZE: u32 = 4;

        let mut library = MaterialLibrary::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let mut vegetation_materials = Assets::<crate::vegetation::VegetationMaterial>::default();
        let mut images = Assets::<Image>::default();
        let mut first_identity: Option<(MaterialId, Handle<StandardMaterial>, Handle<Image>)> =
            None;
        let active_payload_per_image =
            rgba8_mip_payload_bytes(ACTIVE_IMAGE_SIZE, ACTIVE_IMAGE_SIZE).unwrap();

        for round in 0..ROUNDS {
            let first = round * BATCH_SIZE;
            let prepared = (first..first + BATCH_SIZE)
                .map(|identity| {
                    let id = DURABLE_CUSTOM_MATERIAL_BASE + identity as MaterialId;
                    prepared_custom_sized(
                        id,
                        &format!("churn_{identity:04}"),
                        ACTIVE_IMAGE_SIZE,
                        [
                            identity as u8,
                            (identity >> 4) as u8,
                            (identity >> 8) as u8,
                            255,
                        ],
                    )
                })
                .collect();
            library.rebuild_prepared(
                &mut materials,
                &mut vegetation_materials,
                &mut images,
                TEST_SWATCH_SIZE,
                Ok(prepared),
            );

            if round == 0 {
                let id = DURABLE_CUSTOM_MATERIAL_BASE;
                let material = library.handles.get(&id).unwrap().clone();
                let image = materials
                    .get(&material)
                    .unwrap()
                    .base_color_texture
                    .clone()
                    .unwrap();
                first_identity = Some((id, material, image));
            }

            let historical = (round + 1) * BATCH_SIZE;
            let expected_active = BATCH_SIZE * active_payload_per_image;
            let expected_tombstones =
                (historical - BATCH_SIZE) * CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES;
            assert_eq!(library.custom_sources.len(), historical);
            assert_eq!(library.custom_ids.len(), BATCH_SIZE);
            assert_eq!(library.custom_active_image_payload_bytes, expected_active);
            assert_eq!(
                library.custom_tombstone_image_payload_bytes,
                expected_tombstones
            );
            assert_eq!(
                custom_payloads(&library, &materials, &images),
                (expected_active, expected_tombstones)
            );
            assert!(
                expected_active + expected_tombstones <= MAX_CUSTOM_RESIDENT_IMAGE_PAYLOAD_BYTES
            );
        }

        assert_eq!(library.custom_sources.len(), MAX_CUSTOM_MATERIAL_IDENTITIES);
        assert_eq!(materials.len(), 45 + MAX_CUSTOM_MATERIAL_IDENTITIES);
        assert_eq!(images.len(), 45 + MAX_CUSTOM_MATERIAL_IDENTITIES);
        assert_eq!(vegetation_materials.len(), 4);
        let (first_id, first_material, first_image) = first_identity.unwrap();
        assert_eq!(first_id, DURABLE_CUSTOM_MATERIAL_BASE);
        assert_eq!(
            library.handles.get(&first_id).unwrap().id(),
            first_material.id()
        );
        assert_eq!(
            materials
                .get(&first_material)
                .unwrap()
                .base_color_texture
                .as_ref()
                .unwrap()
                .id(),
            first_image.id()
        );
        assert_eq!(images.get(&first_image).unwrap().data.len(), 4);

        let population_at_cap = (materials.len(), vegetation_materials.len(), images.len());
        library.rebuild_prepared(
            &mut materials,
            &mut vegetation_materials,
            &mut images,
            TEST_SWATCH_SIZE,
            Ok(Vec::new()),
        );
        assert_eq!(library.custom_active_image_payload_bytes, 0);
        assert_eq!(
            library.custom_tombstone_image_payload_bytes,
            MAX_CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES
        );
        assert_eq!(
            custom_payloads(&library, &materials, &images),
            (0, MAX_CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES)
        );
        assert_eq!(
            (materials.len(), vegetation_materials.len(), images.len()),
            population_at_cap
        );

        let reactivated = (0..BATCH_SIZE)
            .map(|identity| {
                let id = DURABLE_CUSTOM_MATERIAL_BASE + identity as MaterialId;
                prepared_custom_sized(
                    id,
                    &format!("churn_{identity:04}"),
                    ACTIVE_IMAGE_SIZE,
                    [1, 2, identity as u8, 255],
                )
            })
            .collect();
        library.rebuild_prepared(
            &mut materials,
            &mut vegetation_materials,
            &mut images,
            TEST_SWATCH_SIZE,
            Ok(reactivated),
        );
        assert_eq!(
            (materials.len(), vegetation_materials.len(), images.len()),
            population_at_cap,
            "reactivation at the identity cap must allocate no assets"
        );
        assert_eq!(
            library.handles.get(&first_id).unwrap().id(),
            first_material.id()
        );
        assert_eq!(
            materials
                .get(&first_material)
                .unwrap()
                .base_color_texture
                .as_ref()
                .unwrap()
                .id(),
            first_image.id()
        );
        assert_eq!(
            images.get(&first_image).unwrap().data.len(),
            active_payload_per_image
        );
        assert_eq!(
            custom_payloads(&library, &materials, &images),
            (
                BATCH_SIZE * active_payload_per_image,
                (MAX_CUSTOM_MATERIAL_IDENTITIES - BATCH_SIZE)
                    * CUSTOM_TOMBSTONE_IMAGE_PAYLOAD_BYTES,
            )
        );
    }

    #[test]
    fn invalid_custom_candidate_set_fails_closed_without_partial_remap() {
        let mut library = MaterialLibrary::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let mut vegetation_materials = Assets::<crate::vegetation::VegetationMaterial>::default();
        let mut images = Assets::<Image>::default();
        library.rebuild_prepared(
            &mut materials,
            &mut vegetation_materials,
            &mut images,
            TEST_SWATCH_SIZE,
            Ok(vec![prepared_custom(TEST_SAFE_ID, "safe", [1, 2, 3, 255])]),
        );
        let safe_id = TEST_SAFE_ID;
        let safe_handle = library.handles.get(&safe_id).unwrap().clone();
        let active_before = library.custom_ids.clone();
        let sources_before = library.custom_sources.clone();
        let payload_accounting_before = (
            library.custom_active_image_payload_bytes,
            library.custom_tombstone_image_payload_bytes,
        );
        let safe_image = materials
            .get(&safe_handle)
            .unwrap()
            .base_color_texture
            .clone()
            .unwrap();
        let safe_image_data_before = images.get(&safe_image).unwrap().data.clone();
        let population_before = (materials.len(), vegetation_materials.len(), images.len());

        library.rebuild_prepared(
            &mut materials,
            &mut vegetation_materials,
            &mut images,
            TEST_SWATCH_SIZE,
            Ok(vec![
                prepared_custom(TEST_C_ID, "first", [4, 5, 6, 255]),
                prepared_custom(TEST_C_ID, "second", [7, 8, 9, 255]),
            ]),
        );

        assert!(library.status.contains("Custom-Reload abgelehnt"));
        assert_eq!(library.custom_ids, active_before);
        assert_eq!(library.custom_sources, sources_before);
        assert_eq!(
            (
                library.custom_active_image_payload_bytes,
                library.custom_tombstone_image_payload_bytes,
            ),
            payload_accounting_before
        );
        assert_eq!(
            library.handles.get(&safe_id).unwrap().id(),
            safe_handle.id()
        );
        assert_eq!(
            materials
                .get(&safe_handle)
                .unwrap()
                .base_color_texture
                .as_ref()
                .unwrap()
                .id(),
            safe_image.id()
        );
        assert_eq!(
            images.get(&safe_image).unwrap().data,
            safe_image_data_before
        );
        assert_eq!(
            (materials.len(), vegetation_materials.len(), images.len()),
            population_before
        );
    }

    #[test]
    fn repeating_images_ship_gamma_correct_mips_and_anisotropic_filtering() {
        let pixels = vec![
            0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
        ];
        let image = make_repeating_image(2, 2, pixels);

        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        assert_eq!(image.data.len(), 2 * 2 * 4 + 4);
        assert!(
            (186..=189).contains(&image.data[16]),
            "50% linear light should encode near sRGB 188, got {}",
            image.data[16]
        );
        assert_eq!(&image.data[16..19], &[image.data[16]; 3]);
        assert_eq!(image.data[19], 255);
        let ImageSampler::Descriptor(sampler) = &image.sampler else {
            panic!("repeating terrain images need an explicit sampler");
        };
        assert_eq!(sampler.anisotropy_clamp, 8);
    }

    #[test]
    fn non_power_of_two_mips_include_the_final_source_column() {
        let pixels = vec![0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255];
        let (mips, levels) = rgba8_srgb_mip_chain(3, 1, pixels);

        assert_eq!(levels, 2);
        assert_eq!(mips.len(), 3 * 4 + 4);
        assert!(
            (154..=158).contains(&mips[12]),
            "one white texel out of three should survive the 1x1 mip, got {}",
            mips[12]
        );
    }

    fn luma(pixel: &[u8]) -> u8 {
        ((pixel[0] as u16 * 54 + pixel[1] as u16 * 183 + pixel[2] as u16 * 19) / 256) as u8
    }

    fn luma_range(swatch: &BlockSwatch) -> u8 {
        let mut min = u8::MAX;
        let mut max = u8::MIN;
        for pixel in swatch.rgba.chunks_exact(4) {
            let y = luma(pixel);
            min = min.min(y);
            max = max.max(y);
        }
        max.saturating_sub(min)
    }

    fn luma_standard_deviation(swatch: &BlockSwatch) -> f64 {
        let values: Vec<f64> = swatch
            .rgba
            .chunks_exact(4)
            .map(|pixel| f64::from(luma(pixel)))
            .collect();
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        (values
            .iter()
            .map(|value| (value - mean) * (value - mean))
            .sum::<f64>()
            / values.len() as f64)
            .sqrt()
    }

    fn unique_rgb_count(swatch: &BlockSwatch) -> usize {
        let mut unique = std::collections::BTreeSet::new();
        for pixel in swatch.rgba.chunks_exact(4) {
            unique.insert([pixel[0], pixel[1], pixel[2]]);
        }
        unique.len()
    }

    fn downsample_signature_count(swatch: &BlockSwatch, step: usize) -> usize {
        let mut unique = std::collections::BTreeSet::new();
        let width = swatch.width as usize;
        let height = swatch.height as usize;

        for y in (0..height).step_by(step) {
            for x in (0..width).step_by(step) {
                let mut acc = [0u32; 3];
                let mut count = 0u32;
                for yy in y..(y + step).min(height) {
                    for xx in x..(x + step).min(width) {
                        let i = (yy * width + xx) * 4;
                        acc[0] += swatch.rgba[i] as u32;
                        acc[1] += swatch.rgba[i + 1] as u32;
                        acc[2] += swatch.rgba[i + 2] as u32;
                        count += 1;
                    }
                }
                if count > 0 {
                    unique.insert([
                        (acc[0] / count / 8) as u8,
                        (acc[1] / count / 8) as u8,
                        (acc[2] / count / 8) as u8,
                    ]);
                }
            }
        }

        unique.len()
    }

    fn swatch_for(swatches: &[BlockSwatch], block: BlockType) -> &BlockSwatch {
        swatches
            .iter()
            .find(|swatch| swatch.block == block)
            .expect("block swatch exists")
    }

    #[test]
    fn built_in_materials_keep_scale_appropriate_detail_after_downsampling() {
        assert!(BUILTIN_SWATCH_SIZE >= 128);

        let swatches = bake_all_block_swatches(BUILTIN_SWATCH_SIZE);
        let stone = swatch_for(&swatches, BlockType::Stone);
        let grass = swatch_for(&swatches, BlockType::Grass);
        let lava = swatch_for(&swatches, BlockType::Lava);
        let stone_signatures = downsample_signature_count(stone, 8);
        let grass_signatures = downsample_signature_count(grass, 8);
        let lava_signatures = downsample_signature_count(lava, 8);

        assert!(unique_rgb_count(stone) > 512);
        assert!(
            unique_rgb_count(grass) > 512,
            "near-field grass must retain rich fibre variation"
        );
        assert!(luma_range(stone) > 54);
        assert!(
            stone_signatures > 20,
            "stone only preserved {stone_signatures} far-distance material signatures"
        );
        assert!(
            (6..=16).contains(&grass_signatures),
            "grass should aggregate into calm meadow masses at distance, got {grass_signatures} signatures"
        );
        assert!(
            lava_signatures > 14,
            "lava only preserved {lava_signatures} far-distance material signatures"
        );
    }

    #[test]
    fn karst_materials_are_calmer_than_generic_rock_without_becoming_flat() {
        let limestone = bake_block_swatch(
            BlockType::Limestone,
            "limestone",
            BlockStyle::Rock,
            BUILTIN_SWATCH_SIZE,
        );
        let moss = bake_block_swatch(
            BlockType::MossStone,
            "moss_stone",
            BlockStyle::Rock,
            BUILTIN_SWATCH_SIZE,
        );
        let stone = bake_block_swatch(
            BlockType::Stone,
            "stone",
            BlockStyle::Rock,
            BUILTIN_SWATCH_SIZE,
        );
        let stone_deviation = luma_standard_deviation(&stone);
        let limestone_deviation = luma_standard_deviation(&limestone);
        let moss_deviation = luma_standard_deviation(&moss);

        assert!(
            limestone_deviation < stone_deviation * 0.65,
            "limestone contrast {limestone_deviation:.2} should remain far below generic rock {stone_deviation:.2}"
        );
        assert!(
            moss_deviation < stone_deviation * 0.80,
            "moss contrast {moss_deviation:.2} should remain below generic rock {stone_deviation:.2}"
        );
        assert!(luma_range(&limestone) > 8);
        assert!(luma_range(&moss) > 8);
        assert!(unique_rgb_count(&limestone) > 128);
        assert!(unique_rgb_count(&moss) > 64);
    }

    #[test]
    fn translucent_non_lava_world_materials_avoid_sorted_alpha_blend() {
        for block in [
            BlockType::Water,
            BlockType::Ice,
            BlockType::Crystal,
            BlockType::CockpitGlass,
            BlockType::LuminiteCrystal,
            BlockType::IridiumVein,
        ] {
            assert_eq!(
                terrain_alpha_mode_for_block(block),
                AlphaMode::AlphaToCoverage,
                "{block:?} should stay out of Bevy's sorted alpha-blend terrain path"
            );
        }
    }

    #[test]
    fn lava_material_has_one_bounded_vertex_emission_authority() {
        let mut library = MaterialLibrary::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let mut vegetation_materials = Assets::<crate::vegetation::VegetationMaterial>::default();
        let mut images = Assets::<Image>::default();
        rebuild_without_custom(
            &mut library,
            &mut materials,
            &mut vegetation_materials,
            &mut images,
        );

        let lava = materials
            .get(
                library
                    .handles
                    .get(&(BlockType::Lava as MaterialId))
                    .expect("built-in Lava material handle"),
            )
            .expect("built-in Lava material");
        assert_eq!(lava.emissive, LinearRgba::BLACK);
        assert_eq!(lava.alpha_mode, AlphaMode::Opaque);
        assert_eq!(lava.base_color.to_srgba().alpha, 1.0);
        assert_eq!(
            terrain_alpha_mode_for_block(BlockType::Lava),
            AlphaMode::Opaque
        );
    }

    #[test]
    fn lava_material_split_does_not_mutate_procedural_swatch_detail() {
        let lava = bake_block_swatch(
            BlockType::Lava,
            "lava",
            BlockStyle::Lava,
            BUILTIN_SWATCH_SIZE,
        );
        let signatures_before_material_composition = downsample_signature_count(&lava, 8);

        assert_eq!(lava.rgba.len(), 65_536);
        assert!(unique_rgb_count(&lava) > 512);
        assert!(luma_range(&lava) > 54);
        assert!(
            signatures_before_material_composition > 14,
            "Lava swatch detail changed before StandardMaterial composition"
        );
    }

    #[test]
    fn non_lava_emissive_material_terms_remain_exactly_unchanged() {
        for block in [
            BlockType::Crystal,
            BlockType::NeonCyan,
            BlockType::NeonMagenta,
            BlockType::NeonAmber,
            BlockType::EngineCore,
            BlockType::LuminiteCrystal,
            BlockType::MagnetiteOre,
            BlockType::IridiumVein,
            BlockType::NeonGlass,
            BlockType::ShojiLamp,
        ] {
            assert!(block.is_emissive(), "test list drifted for {block:?}");
            let lin = block.color().to_linear();
            let previous = LinearRgba::rgb(
                lin.red * 3.2 + 0.35,
                lin.green * 3.2 + 0.35,
                lin.blue * 3.2 + 0.35,
            );
            assert_eq!(terrain_material_emissive(block), previous, "{block:?}");
        }
    }

    #[test]
    fn water_material_uses_reflective_low_roughness_profile() {
        let water = terrain_material_profile(BlockType::Water);
        let grass = terrain_material_profile(BlockType::Grass);

        assert!(water.perceptual_roughness < 0.25);
        let water_f0 = 0.16 * water.reflectance * water.reflectance;
        let ior_water = 1.333_f32;
        let expected_f0 = ((ior_water - 1.0) / (ior_water + 1.0)).powi(2);
        assert!((water_f0 - expected_f0).abs() < 0.0001);
        assert!(water.perceptual_roughness < grass.perceptual_roughness);
        assert_eq!(water.metallic, 0.0);
        assert_eq!(water.alpha_mode, AlphaMode::AlphaToCoverage);
        assert_eq!(water.base_alpha, 1.0);
        assert!((0.50..=0.80).contains(&BlockType::Water.color().to_srgba().alpha));
    }

    #[test]
    fn built_in_optical_families_are_finite_bounded_and_distinct() {
        let mut signatures = std::collections::BTreeSet::new();
        for voxel in 1..=BlockType::ShojiLamp as u16 {
            let block = BlockType::from_voxel(voxel);
            let profile = terrain_material_profile(block);
            assert!(profile.base_alpha.is_finite(), "{block:?}");
            assert!(profile.perceptual_roughness.is_finite(), "{block:?}");
            assert!(profile.reflectance.is_finite(), "{block:?}");
            assert!(profile.metallic.is_finite(), "{block:?}");
            assert!((0.0..=1.0).contains(&profile.base_alpha), "{block:?}");
            assert!(
                (0.089..=1.0).contains(&profile.perceptual_roughness),
                "{block:?}"
            );
            assert!((0.0..=1.0).contains(&profile.reflectance), "{block:?}");
            assert!((0.0..=1.0).contains(&profile.metallic), "{block:?}");
            signatures.insert((
                profile.perceptual_roughness.to_bits(),
                profile.reflectance.to_bits(),
                profile.metallic.to_bits(),
            ));
        }

        // The baseline collapsed 38 of 44 built-ins onto one scalar profile.
        // Ten or more signatures prove the family split remains meaningful.
        assert!(
            signatures.len() >= 10,
            "only {} optical signatures remain",
            signatures.len()
        );
    }

    #[test]
    fn optical_family_order_preserves_material_semantics() {
        let dirt = terrain_material_profile(BlockType::Dirt);
        let stone = terrain_material_profile(BlockType::Stone);
        let ice = terrain_material_profile(BlockType::Ice);
        let alloy = terrain_material_profile(BlockType::ShipHullAlloy);
        let glass = terrain_material_profile(BlockType::CockpitGlass);

        assert!(dirt.perceptual_roughness > stone.perceptual_roughness);
        assert!(stone.perceptual_roughness > ice.perceptual_roughness);
        assert!(glass.perceptual_roughness < stone.perceptual_roughness);
        assert_eq!(dirt.metallic, 0.0);
        assert_eq!(stone.metallic, 0.0);
        assert_eq!(glass.metallic, 0.0);
        assert!(alloy.metallic >= 0.85);
        assert!(alloy.perceptual_roughness < stone.perceptual_roughness);
    }

    #[test]
    fn foliage_materials_are_soft_but_not_flat_or_plastic() {
        let leaves = terrain_material_profile(BlockType::Leaves);
        let blossoms = terrain_material_profile(BlockType::BlossomLeaves);
        let stone = terrain_material_profile(BlockType::Stone);

        assert!((0.75..=0.88).contains(&leaves.perceptual_roughness));
        assert!((0.68..leaves.perceptual_roughness).contains(&blossoms.perceptual_roughness));
        assert!((0.30..=0.55).contains(&leaves.reflectance));
        assert_eq!(leaves.metallic, 0.0);
        assert_eq!(stone.metallic, 0.0);
        assert_eq!(leaves.alpha_mode, AlphaMode::Mask(0.42));
        assert_eq!(blossoms.alpha_mode, AlphaMode::Mask(0.42));
    }

    #[test]
    fn foliage_texture_has_restrained_near_field_cutouts() {
        let leaves = bake_block_swatch(
            BlockType::Leaves,
            "leaves",
            BlockStyle::Leaves,
            BUILTIN_SWATCH_SIZE,
        );
        let texels = leaves.rgba.chunks_exact(4).count();
        let holes = leaves
            .rgba
            .chunks_exact(4)
            .filter(|pixel| pixel[3] == 0)
            .count();
        let coverage = holes as f32 / texels as f32;

        assert!(
            (0.035..=0.24).contains(&coverage),
            "foliage cutout coverage {coverage:.3} must add pores without dissolving crowns"
        );
    }

    #[test]
    fn water_ripple_albedo_is_opaque_and_keeps_visible_detail() {
        let swatch = bake_block_swatch(
            BlockType::Water,
            "water",
            BlockStyle::Water,
            BUILTIN_SWATCH_SIZE,
        );

        assert!(swatch.rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
        let signatures = downsample_signature_count(&swatch, 4);
        assert!(
            signatures > 8,
            "water only preserved {signatures} ripple signatures"
        );
        assert!(luma_range(&swatch) > 8);
    }
}
