#!/usr/bin/env python3
"""Build the project-authored Voxel Native Codex Engineering Atlas PDF.

The atlas is intentionally source-first. It renders stable explanatory text,
project diagrams, exact formulas, hard budgets, and explicit acceptance
boundaries. It does not inspect QA directories, select runtime screenshots, or
emit a release verdict. Runtime gallery slots remain visibly pending until a
separate manifest-backed visual acceptance pass exists.
"""

from __future__ import annotations

# A CLI build executes a second, byte-bound inner pass. This minimal outer pass
# runs before argparse or any document dependency is imported, reads this file
# once through a bounded descriptor, and executes exactly the stable bytes it
# captured. Normal module imports skip the trampoline so pure tests can import
# helpers without turning into a CLI build.
import os as _boot_os
import stat as _boot_stat
import sys as _boot_sys


_ATLAS_BOUND_BUILDER_BYTES = globals().get("_ATLAS_BOUND_BUILDER_BYTES")
_ATLAS_BOOT_MAX_SOURCE_BYTES = 8 * 1024 * 1024


def _atlas_bootstrap_fail(message: str) -> None:
    print(f"atlas build failed during source binding: {message}", file=_boot_sys.stderr)
    raise SystemExit(2)


if __name__ == "__main__" and _ATLAS_BOUND_BUILDER_BYTES is None:
    _builder_path = _boot_os.path.abspath(__file__)
    _flags = (
        _boot_os.O_RDONLY
        | getattr(_boot_os, "O_BINARY", 0)
        | getattr(_boot_os, "O_NOFOLLOW", 0)
    )
    try:
        _descriptor = _boot_os.open(_builder_path, _flags)
    except OSError as _error:
        _atlas_bootstrap_fail(f"could not open builder safely: {_error}")

    try:
        _before = _boot_os.fstat(_descriptor)
        _reparse_flag = getattr(_boot_stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
        if (
            not _boot_stat.S_ISREG(_before.st_mode)
            or _boot_stat.S_ISLNK(_before.st_mode)
            or bool(getattr(_before, "st_file_attributes", 0) & _reparse_flag)
        ):
            _atlas_bootstrap_fail("builder is not a safe regular file")
        if _before.st_size <= 0 or _before.st_size > _ATLAS_BOOT_MAX_SOURCE_BYTES:
            _atlas_bootstrap_fail(
                f"builder size is outside the {_ATLAS_BOOT_MAX_SOURCE_BYTES}-byte cap"
            )

        _chunks: list[bytes] = []
        _remaining = _ATLAS_BOOT_MAX_SOURCE_BYTES + 1
        while _remaining > 0:
            _chunk = _boot_os.read(_descriptor, min(1_048_576, _remaining))
            if not _chunk:
                break
            _chunks.append(_chunk)
            _remaining -= len(_chunk)
        _captured_builder_bytes = b"".join(_chunks)
        _after = _boot_os.fstat(_descriptor)
    except OSError as _error:
        _atlas_bootstrap_fail(f"could not read builder safely: {_error}")
    finally:
        _boot_os.close(_descriptor)

    _stable_before = (
        _before.st_dev,
        _before.st_ino,
        _before.st_size,
        _before.st_mtime_ns,
        _before.st_ctime_ns,
    )
    _stable_after = (
        _after.st_dev,
        _after.st_ino,
        _after.st_size,
        _after.st_mtime_ns,
        _after.st_ctime_ns,
    )
    if (
        _stable_before != _stable_after
        or len(_captured_builder_bytes) != _before.st_size
        or len(_captured_builder_bytes) > _ATLAS_BOOT_MAX_SOURCE_BYTES
    ):
        _atlas_bootstrap_fail("builder changed while its execution bytes were captured")
    try:
        _final = _boot_os.lstat(_builder_path)
    except OSError as _error:
        _atlas_bootstrap_fail(f"could not revalidate builder identity: {_error}")
    if (
        _boot_stat.S_ISLNK(_final.st_mode)
        or bool(getattr(_final, "st_file_attributes", 0) & _reparse_flag)
        or not _boot_os.path.samestat(_before, _final)
    ):
        _atlas_bootstrap_fail("builder identity changed before bound execution")

    try:
        _bound_code = compile(
            _captured_builder_bytes,
            _builder_path,
            "exec",
            dont_inherit=True,
        )
    except (SyntaxError, ValueError) as _error:
        _atlas_bootstrap_fail(f"could not compile captured builder bytes: {_error}")
    _bound_globals = {
        "__builtins__": __builtins__,
        "__cached__": None,
        "__file__": _builder_path,
        "__loader__": None,
        "__name__": "__main__",
        "__package__": None,
        "__spec__": None,
        "_ATLAS_BOUND_BUILDER_BYTES": bytes(_captured_builder_bytes),
    }
    exec(_bound_code, _bound_globals, _bound_globals)
    _atlas_bootstrap_fail("bound builder execution returned without an exit status")

import argparse
import hashlib
import html
import io
import math
import os
import re
import stat
import sys
import tempfile
import warnings
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Sequence


# `--check-only` is a strict no-artifact route.  Keep lazy third-party imports
# from creating bytecode beside an otherwise read-only dependency installation.
sys.dont_write_bytecode = True


DEFAULT_OUTPUT = Path("output/pdf/voxel-native-codex-engineering-atlas.pdf")
CANONICAL_RELEASE_PDF = Path(
    "docs/releases/technical-preview/voxel-native-codex-engineering-atlas.pdf"
)
EXPECTED_PAGE_COUNT = 15
MAX_SOURCE_BYTES = _ATLAS_BOOT_MAX_SOURCE_BYTES
MAX_TOTAL_SOURCE_BYTES = 64 * 1024 * 1024
MAX_PDF_BYTES = 64 * 1024 * 1024
MAX_SVG_NODES = 8_192
MAX_SVG_DEPTH = 64
MAX_SVG_ATTRIBUTES = 65_536
MAX_SVG_PATH_CHARACTERS = 1_048_576
SVG_PREFLIGHT_CHUNK_BYTES = 64 * 1024
MAX_PDF_OBJECTS = 50_000
FORBIDDEN_PDF_KEYS = frozenset(
    {
        "/AA",
        "/AcroForm",
        "/Dur",
        "/EF",
        "/EmbeddedFiles",
        "/FontFile",
        "/FontFile2",
        "/FontFile3",
        "/JavaScript",
        "/JS",
        "/OpenAction",
        "/Outlines",
        "/RichMediaContent",
        "/RichMediaSettings",
        "/Trans",
        "/XFA",
    }
)
FORBIDDEN_PDF_ACTIONS = frozenset(
    {
        "/GoToE",
        "/GoToR",
        "/GoTo3DView",
        "/Hide",
        "/ImportData",
        "/JavaScript",
        "/Launch",
        "/Movie",
        "/Named",
        "/Rendition",
        "/ResetForm",
        "/SetOCGState",
        "/Sound",
        "/SubmitForm",
        "/Thread",
        "/Trans",
    }
)
FORBIDDEN_PDF_TYPES = frozenset(
    {
        "/3D",
        "/EmbeddedFile",
        "/Filespec",
        "/Movie",
        "/PS",
        "/RichMedia",
        "/Screen",
        "/Sound",
    }
)
SAFE_SVG_TAGS = frozenset(
    {
        "circle",
        "defs",
        "desc",
        "feGaussianBlur",
        "feMerge",
        "feMergeNode",
        "filter",
        "g",
        "linearGradient",
        "marker",
        "path",
        "pattern",
        "radialGradient",
        "rect",
        "stop",
        "style",
        "svg",
        "text",
        "title",
    }
)
SAFE_SVG_ATTRIBUTES = frozenset(
    {
        "aria-labelledby",
        "baseline-shift",
        "class",
        "cx",
        "cy",
        "d",
        "fill",
        "fill-opacity",
        "filter",
        "font-family",
        "font-size",
        "font-weight",
        "height",
        "id",
        "in",
        "letter-spacing",
        "marker-end",
        "markerHeight",
        "markerWidth",
        "offset",
        "opacity",
        "orient",
        "patternUnits",
        "r",
        "refX",
        "refY",
        "result",
        "role",
        "rx",
        "stdDeviation",
        "stop-color",
        "stop-opacity",
        "stroke",
        "stroke-dasharray",
        "stroke-linecap",
        "stroke-linejoin",
        "stroke-opacity",
        "stroke-width",
        "text-anchor",
        "transform",
        "viewBox",
        "width",
        "x",
        "x1",
        "x2",
        "y",
        "y1",
        "y2",
    }
)
NUMERIC_SVG_ATTRIBUTES = frozenset(
    {
        "baseline-shift",
        "cx",
        "cy",
        "d",
        "fill-opacity",
        "font-size",
        "height",
        "letter-spacing",
        "markerHeight",
        "markerWidth",
        "offset",
        "opacity",
        "orient",
        "r",
        "refX",
        "refY",
        "rx",
        "stdDeviation",
        "stop-opacity",
        "stroke-dasharray",
        "stroke-opacity",
        "stroke-width",
        "transform",
        "viewBox",
        "width",
        "x",
        "x1",
        "x2",
        "y",
        "y1",
        "y2",
    }
)
SAFE_SVG_CSS_PROPERTIES = frozenset(
    {
        "baseline-shift",
        "fill",
        "fill-opacity",
        "filter",
        "font",
        "font-family",
        "font-size",
        "font-style",
        "font-weight",
        "letter-spacing",
        "marker-end",
        "opacity",
        "stop-color",
        "stop-opacity",
        "stroke",
        "stroke-dasharray",
        "stroke-linecap",
        "stroke-linejoin",
        "stroke-opacity",
        "stroke-width",
        "text-anchor",
        "transform",
    }
)
SVG_INPUT_FONT_FAMILIES = frozenset({"Courier", "Helvetica"})
CANONICAL_SVG_FONT_FAMILIES = frozenset(
    {
        "Courier",
        "Courier-Bold",
        "Courier-BoldOblique",
        "Courier-Oblique",
        "Helvetica",
        "Helvetica-Bold",
        "Helvetica-BoldOblique",
        "Helvetica-Oblique",
    }
)
ALLOWED_PDF_BASE_FONT_NAMES = frozenset(
    {f"/{family}" for family in CANONICAL_SVG_FONT_FAMILIES} | {"/Times-Roman"}
)
SVG_NUMBER_PATTERN = r"[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?"
SVG_LENGTH_PATTERN = rf"{SVG_NUMBER_PATTERN}(?:px|pt|pc|mm|cm|in|em|ex|%)?"
BUILDER_SOURCE = "tools/artifacts/build_codex_engineering_atlas.py"
UNICODE_DASHES = "\u2010\u2011\u2012\u2013\u2014\u2212"
SVG_ASCII_REPLACEMENTS = {
    "\u00b1": "+/-",
    "\u00b2": "^2",
    "\u00b3": "^3",
    "\u00b7": " | ",
    "\u00d7": "x",
    "\u0394": "Delta",
    "\u2010": "-",
    "\u2011": "-",
    "\u2012": "-",
    "\u2013": "-",
    "\u2014": "-",
    "\u2016": "||",
    "\u201c": "&quot;",
    "\u201d": "&quot;",
    "\u2022": "-",
    "\u2026": "...",
    "\u2113": "l",
    "\u2192": "-&gt;",
    "\u2212": "-",
    "\u221e": "INF",
    "\u2264": "&lt;=",
    "\u2265": ">=",
}
CANONICAL_PDF_DATE = "D:20000101000000+00'00'"
PDF_METADATA = {
    "title": "Voxel Native Codex Engineering Atlas",
    "author": "Voxel Native project",
    "creator": "Voxel Native project-authored ReportLab builder",
    "producer": "Voxel Native deterministic atlas builder",
    "subject": "Source-first technical atlas; no runtime release verdict; runtime gallery pending",
    "keywords": "Voxel Native, Codex, voxel engine, bounded systems, technical atlas",
}
EXPECTED_PAGE_HEADINGS: tuple[str, ...] = (
    "CODEX ENGINEERING ATLAS",
    "A technical map, not a cinematic promise",
    "One world, several bounded views",
    "Geometric reach, fixed representation",
    "Seam morphing without planetary float identity",
    "Toroidal sampling and a fail-closed Near handoff",
    "Hydrography, river-bank grammar, and sparse silhouettes",
    "Fixed-memory virtual bricks",
    "Road-first city planning math",
    "Technical truth is separate from presentation",
    "Every ambitious system publishes its stop condition",
    "Research becomes reversible engine work",
    "Ideas enter as sources; claims leave through gates",
    "Runtime gallery pending",
    "Official routes and final artifact checklist",
)
HYDRO_ROLLBACK_LABEL = "Hydro gate off"
COHORT_ROLLBACK_LABEL = "Cohort gate off"
ROAD_GRADE_FIT_FORMULA = (
    "route_fit = clamp(\n"
    "  1 - 0.55*clamp(avg_step/5,0,1)\n"
    "    - 0.30*clamp(max_step/9,0,1)\n"
    "    - 0.15*clamp(\n"
    "        max(height_range-18,0)/34,0,1),\n"
    "  0,1)"
)


@dataclass(frozen=True)
class InputSnapshot:
    relative: str
    path: Path
    data: bytes
    text: str
    sha256: str

    @property
    def size(self) -> int:
        return len(self.data)


@dataclass(frozen=True)
class OutputTarget:
    path: Path
    allowed_root: Path


@dataclass(frozen=True)
class BuiltPdf:
    data: bytes
    sha256: str
    document_id_hex: str


@dataclass(frozen=True)
class AtlasDocumentIdentity:
    fingerprint: str
    builder_sha: str
    toolchain_identity: str
    document_id_hex: str


REQUIRED_ASSETS: tuple[str, ...] = (
    "docs/media/voxel-native-hero.svg",
    "docs/media/world-representation-architecture.svg",
    "docs/media/planetary-budget-envelope.svg",
    "docs/media/research-routes.svg",
    "docs/media/toroidal-cache-reuse.svg",
    "docs/media/river-bank-v3-cross-section.svg",
    "docs/media/city-site-score.svg",
    "docs/media/evidence-lineage.svg",
)

SOURCE_CONTRACTS: dict[str, tuple[str, ...]] = {
    "README.md": (
        "What is real today",
        "Codex engineering loop",
        "Research is visible",
    ),
    "docs/CODEX_ENGINEERING_ATLAS.md": (
        "Reading the status labels",
        "Formula index",
        "The promotion rule",
    ),
    "docs/ARTIFACT_EVIDENCE_BUILDERS.md": (
        "Safety and acceptance boundary",
        "Required final-artifact QA",
    ),
    "docs/EVIDENCE_GRAPH_CONTRACT.md": (
        "Authoritative node identity",
        "Fixed limits",
        "Failure behavior",
    ),
    "docs/EVIDENCE_MANIFEST_SCHEMA.md": (
        "Input and output contract",
        "Bounded RON parsing",
        "Downstream rules",
    ),
    "docs/ELITE_WORLD_SYSTEMS_STANDARD.md": (
        "Acceptance ladder",
        "Novel-solution decision record",
    ),
    "docs/RESPONSIVE_VISUAL_QA.md": (
        "Required viewport matrix",
        "Proof recorded with a change",
    ),
    "docs/PLANETARY_STREAMING_ARCHITECTURE.md": (
        "Far terrain: geometry clipmaps",
        "Failure containment",
    ),
    "docs/FAR_TERMINAL_SKIRTS_V1.md": (
        "Fixed population and proof matrix",
        "Failure mode, visual gate, and rollback",
    ),
    "docs/VIRTUAL_VOXEL_HIERARCHY.md": (
        "production resident cap",
        "Self-budgeted residency",
    ),
    "docs/CITY_PLANNER_MATH.md": (
        "Site Score",
        "Low-End Budget",
    ),
    "docs/FAR_HYDROGRAPHIC_CONTINUITY_V1.md": (
        "Compile-time budgets",
        "Failure modes and rollback boundaries",
    ),
    "docs/FAR_SEMANTIC_COHORTS_V1.md": (
        "Exact compile-time budgets",
        "Visual acceptance therefore remains open",
    ),
    "docs/NATURAL_RIVER_BANK_V3.md": (
        "Selected formula",
        "Fixed budgets and failure mode",
        "Deterministic acceptance contract",
    ),
    "docs/VOXEL_DISCOVERY_ATLAS.md": (
        "Evidence policy",
        "Reproducible study protocol",
        "Primary and official sources studied so far",
    ),
    "docs/FAR_WORLD_RENDERING_RESEARCH.md": (
        "Decision in one paragraph",
        "Staged implementation with non-negotiable gates",
        "Candid current limitations and rejection triggers",
    ),
    "src/chunk.rs": (
        "pub const CHUNK_SIZE: usize = 16;",
        "let cx = wx.div_euclid(CHUNK_SIZE_I);",
        "let lx = wx.rem_euclid(CHUNK_SIZE_I) as usize;",
    ),
    "src/bots.rs": (
        ".filter(|origin| bounds.contains_box(*origin, size))",
        ".filter(|origin| project_anchor_loaded(world, *origin, size))",
        ".filter(|origin| !project_footprint_reserved(save, *origin, size, kind))",
        "!project_footprint_blocks_road_corridor(save, district, *origin, size, kind)",
        ".filter(|origin| !road_project_blocks_city_footprint(save, *origin, size, kind))",
        "!road_project_duplicates_existing_corridor(save, district, *origin, size, kind)",
        "fn score_city_slot_with_route_fit(",
        "base + road_route_fit.clamp(0.0, 1.0) * 1.35",
        "let average_penalty = (average_step / 5.0).clamp(0.0, 1.0);",
        "let range_penalty = (((max - min) - 18).max(0) as f32 / 34.0).clamp(0.0, 1.0);",
    ),
    "src/city.rs": (
        "const ROAD_MAX_CENTERLINE_SAMPLES: usize = 513;",
        "fn smoothstep(t: f32) -> f32 {",
        "t * t * (3.0 - 2.0 * t)",
    ),
    "src/agent_control.rs": (
        "const AGENT_CONTROL_MAX_BYTES: usize = 64 * 1024;",
        "Some(last) if sequence > last => AgentControlPayloadDecision::Apply,",
        "if !position.is_finite() || !request.yaw.is_finite() || !request.pitch.is_finite() {",
        "&& !agent_native_metadata_is_reparse_point(&metadata)",
    ),
    BUILDER_SOURCE: (
        "EXPECTED_PAGE_COUNT = 15",
        "RUNTIME GALLERY PENDING",
        "InvariantCanvas",
    ),
}
EXPECTED_INPUT_COUNT = len(REQUIRED_ASSETS) + len(SOURCE_CONTRACTS)

OFFICIAL_REFERENCES: tuple[tuple[str, str, str], ...] = (
    (
        "Rust language",
        "Ownership, checked arithmetic, deterministic native systems",
        "https://www.rust-lang.org/",
    ),
    (
        "Rust Euclidean division",
        "Signed chunk, ring, brick, and supertile mapping",
        "https://doc.rust-lang.org/std/primitive.i32.html#method.div_euclid",
    ),
    (
        "Bevy",
        "ECS, application lifecycle, assets, and native rendering integration",
        "https://bevyengine.org/",
    ),
    (
        "wgpu",
        "Portable graphics abstraction used by the native renderer",
        "https://wgpu.rs/",
    ),
    (
        "GPU Gems 2 - Geometry Clipmaps",
        "Primary terrain-LOD research route; not a runtime equivalence claim",
        "https://developer.nvidia.com/gpugems/gpugems2/part-i-geometric-complexity/chapter-2-terrain-rendering-using-gpu-based-geometry",
    ),
    (
        "Virtual Horizon Method - IBPSA",
        "Directional horizon-query research input",
        "https://publications.ibpsa.org/conference/paper/?id=bs2025_1302",
    ),
    (
        "Multiscale pine rendering - Graphics Interface",
        "Scale-dependent natural-detail research input",
        "https://graphicsinterface.org/proceedings/gi2000/gi2000-19/",
    ),
    (
        "Generative Adversarial Shaders - arXiv",
        "Shader decomposition and ablation-discipline research input",
        "https://arxiv.org/abs/2306.04629",
    ),
    (
        "NIST FIPS 180-4",
        "SHA-256 reference for deterministic evidence identity",
        "https://csrc.nist.gov/pubs/fips/180-4/upd1/final",
    ),
)
OFFICIAL_URIS = frozenset(url for _, _, url in OFFICIAL_REFERENCES)


class AtlasBuildError(RuntimeError):
    """A bounded, user-readable atlas build failure."""


def repository_root() -> Path:
    return Path(__file__).resolve().parents[2]


def is_relative_to(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
    except ValueError:
        return False
    return True


def lexical_absolute(path: Path) -> Path:
    """Normalize `.`/`..` without using symlink resolution."""

    return Path(os.path.abspath(os.fspath(path)))


def is_reparse_stat(info: os.stat_result) -> bool:
    attributes = getattr(info, "st_file_attributes", 0)
    reparse_flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return stat.S_ISLNK(info.st_mode) or bool(attributes & reparse_flag)


def lstat_or_none(path: Path) -> os.stat_result | None:
    try:
        return os.lstat(path)
    except FileNotFoundError:
        return None
    except OSError as error:
        raise AtlasBuildError(f"could not inspect path safely: {path}: {error}") from error


def assert_no_reparse_components(
    path: Path,
    boundary: Path,
    *,
    allow_missing: bool,
    label: str,
) -> None:
    path = lexical_absolute(path)
    boundary = lexical_absolute(boundary)
    if not is_relative_to(path, boundary):
        raise AtlasBuildError(f"{label} escapes its allowed root: {path}")

    current = boundary
    boundary_info = lstat_or_none(boundary)
    if boundary_info is not None and is_reparse_stat(boundary_info):
        raise AtlasBuildError(f"{label} root is a symlink or reparse point: {boundary}")
    for part in path.relative_to(boundary).parts:
        current /= part
        info = lstat_or_none(current)
        if info is None:
            if allow_missing:
                continue
            raise AtlasBuildError(f"missing required {label}: {current}")
        if is_reparse_stat(info):
            raise AtlasBuildError(f"{label} uses a symlink or reparse point: {current}")


def read_input_snapshot(root: Path, relative: str, label: str) -> InputSnapshot:
    relative_path = Path(relative)
    if relative_path.is_absolute() or ".." in relative_path.parts:
        raise AtlasBuildError(f"required {label} has an unsafe relative path: {relative}")
    path = lexical_absolute(root / relative_path)
    assert_no_reparse_components(
        path,
        root,
        allow_missing=False,
        label=label,
    )

    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise AtlasBuildError(f"could not open required {label} safely: {path}: {error}") from error

    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or is_reparse_stat(before):
            raise AtlasBuildError(f"required {label} is not a safe regular file: {path}")
        if before.st_size <= 0:
            raise AtlasBuildError(f"required {label} is empty: {path}")
        if before.st_size > MAX_SOURCE_BYTES:
            raise AtlasBuildError(
                f"required {label} exceeds the {MAX_SOURCE_BYTES}-byte input cap: {path}"
            )

        chunks: list[bytes] = []
        remaining = MAX_SOURCE_BYTES + 1
        while remaining > 0:
            chunk = os.read(descriptor, min(1_048_576, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        after = os.fstat(descriptor)
    except OSError as error:
        raise AtlasBuildError(f"could not read required {label} safely: {path}: {error}") from error
    finally:
        os.close(descriptor)

    if len(data) > MAX_SOURCE_BYTES:
        raise AtlasBuildError(
            f"required {label} exceeds the {MAX_SOURCE_BYTES}-byte input cap: {path}"
        )
    stable_fields_before = (
        before.st_dev,
        before.st_ino,
        before.st_size,
        before.st_mtime_ns,
        before.st_ctime_ns,
    )
    stable_fields_after = (
        after.st_dev,
        after.st_ino,
        after.st_size,
        after.st_mtime_ns,
        after.st_ctime_ns,
    )
    if stable_fields_before != stable_fields_after or len(data) != before.st_size:
        raise AtlasBuildError(f"required {label} changed while it was being read: {path}")

    assert_no_reparse_components(
        path,
        root,
        allow_missing=False,
        label=label,
    )
    final_info = lstat_or_none(path)
    if final_info is None or not os.path.samestat(before, final_info):
        raise AtlasBuildError(f"required {label} identity changed while it was being read: {path}")
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise AtlasBuildError(f"required {label} is not UTF-8: {path}") from error
    return InputSnapshot(
        relative=relative,
        path=path,
        data=data,
        text=text,
        sha256=hashlib.sha256(data).hexdigest(),
    )


def read_stable_bounded_bytes(
    path: Path,
    boundary: Path,
    *,
    byte_limit: int,
    label: str,
) -> bytes:
    """Read one immutable regular-file snapshot without following reparses."""

    if type(byte_limit) is not int or byte_limit <= 0:
        raise AtlasBuildError(f"{label} has an invalid byte cap")
    path = lexical_absolute(path)
    boundary = lexical_absolute(boundary)
    assert_no_reparse_components(
        path,
        boundary,
        allow_missing=False,
        label=label,
    )

    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise AtlasBuildError(f"could not open required {label} safely: {path}: {error}") from error

    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or is_reparse_stat(before):
            raise AtlasBuildError(f"required {label} is not a safe regular file: {path}")
        if before.st_size <= 0:
            raise AtlasBuildError(f"required {label} is empty: {path}")
        if before.st_size > byte_limit:
            raise AtlasBuildError(
                f"required {label} exceeds the {byte_limit}-byte input cap: {path}"
            )

        chunks: list[bytes] = []
        remaining = byte_limit + 1
        while remaining > 0:
            chunk = os.read(descriptor, min(1_048_576, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        after = os.fstat(descriptor)
    except OSError as error:
        raise AtlasBuildError(f"could not read required {label} safely: {path}: {error}") from error
    finally:
        os.close(descriptor)

    if len(data) > byte_limit:
        raise AtlasBuildError(
            f"required {label} exceeds the {byte_limit}-byte input cap: {path}"
        )
    stable_fields_before = (
        before.st_dev,
        before.st_ino,
        before.st_size,
        before.st_mtime_ns,
        before.st_ctime_ns,
    )
    stable_fields_after = (
        after.st_dev,
        after.st_ino,
        after.st_size,
        after.st_mtime_ns,
        after.st_ctime_ns,
    )
    if stable_fields_before != stable_fields_after or len(data) != before.st_size:
        raise AtlasBuildError(f"required {label} changed while it was being read: {path}")

    assert_no_reparse_components(
        path,
        boundary,
        allow_missing=False,
        label=label,
    )
    final_info = lstat_or_none(path)
    if final_info is None or not os.path.samestat(before, final_info):
        raise AtlasBuildError(f"required {label} identity changed while it was being read: {path}")
    return data


def read_canonical_release_pdf(root: Path) -> tuple[Path, bytes]:
    path = lexical_absolute(root / CANONICAL_RELEASE_PDF)
    data = read_stable_bounded_bytes(
        path,
        root,
        byte_limit=MAX_PDF_BYTES,
        label="canonical release PDF",
    )
    return path, data


def bound_builder_snapshot(root: Path, bound_builder_bytes: bytes | None) -> InputSnapshot:
    """Create the self-source identity from the exact bytes executing the CLI."""

    expected_path = lexical_absolute(root / BUILDER_SOURCE)
    executed_path = lexical_absolute(Path(__file__))
    if executed_path != expected_path:
        raise AtlasBuildError(
            "byte-bound atlas builder must execute from its canonical repository path"
        )
    assert_no_reparse_components(
        executed_path,
        root,
        allow_missing=False,
        label="atlas builder execution",
    )
    if type(bound_builder_bytes) is not bytes:
        raise AtlasBuildError("CLI atlas validation requires byte-bound builder source")
    if not bound_builder_bytes or len(bound_builder_bytes) > MAX_SOURCE_BYTES:
        raise AtlasBuildError(
            f"byte-bound builder source is outside the {MAX_SOURCE_BYTES}-byte input cap"
        )
    try:
        text = bound_builder_bytes.decode("utf-8")
    except UnicodeDecodeError as error:
        raise AtlasBuildError("byte-bound builder source is not UTF-8") from error
    return InputSnapshot(
        relative=BUILDER_SOURCE,
        path=lexical_absolute(root / BUILDER_SOURCE),
        data=bound_builder_bytes,
        text=text,
        sha256=hashlib.sha256(bound_builder_bytes).hexdigest(),
    )


def xml_local_name(name: str) -> str:
    return name.rsplit("}", 1)[-1]


def validate_internal_svg_references(value: str, relative: str) -> None:
    lowered = value.lower()
    if "javascript:" in lowered or "@import" in lowered or "expression(" in lowered:
        raise AtlasBuildError(f"required SVG asset contains active CSS or script: {relative}")
    for match in re.finditer(r"url\(\s*(['\"]?)(.*?)\1\s*\)", value, flags=re.IGNORECASE):
        reference = match.group(2).strip()
        if not reference.startswith("#"):
            raise AtlasBuildError(
                f"required SVG asset contains an external URL reference: {relative}"
            )


def guard_resolved_svg_scalar(value: str, relative: str, context: str) -> None:
    """Reject controls and invalid Unicode after XML character references resolve."""

    for character in value:
        codepoint = ord(character)
        if (
            (codepoint < 0x20 and character not in "\t\n\r")
            or 0x7F <= codepoint <= 0x9F
            or codepoint == 0xFFFD
            or 0xFDD0 <= codepoint <= 0xFDEF
            or codepoint & 0xFFFF in {0xFFFE, 0xFFFF}
            or 0xD800 <= codepoint <= 0xDFFF
        ):
            raise AtlasBuildError(
                f"required SVG asset resolves forbidden character U+{codepoint:04X} "
                f"in {context}: {relative}"
            )


def validate_svg_numeric_value(name: str, value: str, relative: str) -> None:
    numeric_values = re.findall(SVG_NUMBER_PATTERN, value)
    for numeric in numeric_values:
        number = float(numeric)
        if not math.isfinite(number) or abs(number) > 10_000_000:
            raise AtlasBuildError(
                f"required SVG asset contains an out-of-range number: {relative}"
            )
        if name == "stdDeviation" and abs(number) > 1_024:
            raise AtlasBuildError(
                f"required SVG asset contains an excessive blur radius: {relative}"
            )


def validate_canonical_svg_font_family(value: str, relative: str) -> None:
    family = value.strip()
    lowered = family.lower()
    if (
        not family
        or family not in SVG_INPUT_FONT_FAMILIES
        or any(character in family for character in "\\/:,()\"'")
        or re.search(r"(?i)\b(?:local|url)\s*\(", family)
        or lowered.startswith(("file:", "data:", "http:", "https:"))
    ):
        allowed = ", ".join(sorted(SVG_INPUT_FONT_FAMILIES))
        raise AtlasBuildError(
            f"required SVG asset uses a non-canonical font family; allowed={allowed}: {relative}"
        )


def validate_svg_font_weight(value: str, relative: str) -> None:
    weight = value.strip()
    if weight in {"normal", "bold"}:
        return
    raise AtlasBuildError(f"required SVG asset uses an unsupported font weight: {relative}")


def validate_svg_font_shorthand(value: str, relative: str) -> None:
    if any(token in value for token in ("\\", ",", "'", '"')):
        raise AtlasBuildError(
            f"required SVG asset uses a non-canonical font shorthand: {relative}"
        )
    size_pattern = re.compile(
        rf"(?<!\S)(?P<size>{SVG_NUMBER_PATTERN})(?P<unit>px|pt)"
        rf"(?:\s*/\s*(?P<line>{SVG_NUMBER_PATTERN})(?P<line_unit>px|pt|%)?)?(?=\s)"
    )
    matches = list(size_pattern.finditer(value))
    if len(matches) != 1:
        raise AtlasBuildError(
            f"required SVG asset uses a non-canonical font shorthand: {relative}"
        )
    match = matches[0]
    size = float(match.group("size"))
    line_height = float(match.group("line")) if match.group("line") is not None else None
    if (
        not math.isfinite(size)
        or size <= 0
        or size > 10_000_000
        or (
            line_height is not None
            and (not math.isfinite(line_height) or line_height <= 0 or line_height > 10_000_000)
        )
    ):
        raise AtlasBuildError(
            f"required SVG asset contains an out-of-range number: {relative}"
        )

    prefix = value[: match.start()].strip().split()
    if len(prefix) > 2:
        raise AtlasBuildError(
            f"required SVG asset uses a non-canonical font shorthand: {relative}"
        )
    seen_style = False
    seen_weight = False
    seen_normal = False
    for token in prefix:
        if token in {"italic", "oblique"}:
            if seen_style:
                raise AtlasBuildError(
                    f"required SVG asset uses a non-canonical font shorthand: {relative}"
                )
            seen_style = True
        elif token == "normal":
            if seen_normal:
                raise AtlasBuildError(
                    f"required SVG asset uses a non-canonical font shorthand: {relative}"
                )
            seen_normal = True
        else:
            if seen_weight:
                raise AtlasBuildError(
                    f"required SVG asset uses a non-canonical font shorthand: {relative}"
                )
            validate_svg_font_weight(token, relative)
            seen_weight = True

    validate_canonical_svg_font_family(value[match.end() :].strip(), relative)


def validate_svg_css_numeric_syntax(name: str, value: str, relative: str) -> None:
    validate_svg_numeric_value(name, value, relative)
    length = rf"(?:{SVG_LENGTH_PATTERN})"
    if name == "transform":
        separator = r"(?:\s*,\s*|\s+)"
        function = (
            rf"(?:matrix|translate|scale|rotate|skewX|skewY)"
            rf"\(\s*{SVG_NUMBER_PATTERN}(?:{separator}{SVG_NUMBER_PATTERN})*\s*\)"
        )
        valid = re.fullmatch(rf"{function}(?:\s+{function})*", value) is not None
    elif name == "stroke-dasharray":
        valid = value == "none" or re.fullmatch(
            rf"{length}(?:(?:\s*,\s*|\s+){length})*", value
        ) is not None
    elif name == "baseline-shift":
        valid = value in {"baseline", "sub", "super"} or re.fullmatch(length, value) is not None
    elif name == "letter-spacing":
        valid = value == "normal" or re.fullmatch(length, value) is not None
    elif name in {"fill-opacity", "opacity", "stop-opacity", "stroke-opacity"}:
        valid = re.fullmatch(rf"{SVG_NUMBER_PATTERN}%?", value) is not None
    else:
        valid = re.fullmatch(length, value) is not None
    if not valid:
        raise AtlasBuildError(
            f"required SVG asset uses unsupported CSS numeric syntax for {name}: {relative}"
        )
    if name in {"font-size", "stroke-width"}:
        number = float(re.search(SVG_NUMBER_PATTERN, value).group(0))
        if number <= 0:
            raise AtlasBuildError(
                f"required SVG asset uses a non-positive CSS {name}: {relative}"
            )


def validate_svg_environment_free_value(value: str, relative: str) -> None:
    if (
        "\\" in value
        or "/*" in value
        or "*/" in value
        or re.search(r"(?i)\b(?:attr|env|image-set|local|var)\s*\(", value)
    ):
        raise AtlasBuildError(
            f"required SVG asset contains an escaped or environment-dependent value: {relative}"
        )


def validate_svg_paint_syntax(value: str, relative: str) -> None:
    if (
        re.fullmatch(r"#[0-9A-Fa-f]{3,8}", value) is None
        and value not in {"none", "transparent"}
        and re.fullmatch(r"url\(#[A-Za-z_][A-Za-z0-9_.:-]*\)", value) is None
    ):
        raise AtlasBuildError(f"required SVG asset uses unsupported paint syntax: {relative}")


def validate_svg_fragment_reference_syntax(value: str, relative: str) -> None:
    if value != "none" and re.fullmatch(
        r"url\(#[A-Za-z_][A-Za-z0-9_.:-]*\)", value
    ) is None:
        raise AtlasBuildError(
            f"required SVG asset uses unsupported fragment-reference syntax: {relative}"
        )


def validate_svg_css_declaration(name: str, value: str, relative: str) -> None:
    if name not in SAFE_SVG_CSS_PROPERTIES:
        raise AtlasBuildError(
            f"required SVG asset contains unsupported CSS property {name!r}: {relative}"
        )
    if not value:
        raise AtlasBuildError(f"required SVG asset contains an empty CSS value: {relative}")
    validate_internal_svg_references(value, relative)
    validate_svg_environment_free_value(value, relative)

    if name == "font":
        validate_svg_font_shorthand(value, relative)
        raise AtlasBuildError(
            f"required SVG asset uses converter-unstable CSS font shorthand; "
            f"use explicit font-family/font-size/font-weight properties: {relative}"
        )
    elif name == "font-family":
        validate_canonical_svg_font_family(value, relative)
    elif name == "font-weight":
        validate_svg_font_weight(value, relative)
    elif name == "font-style":
        if value not in {"normal", "italic", "oblique"}:
            raise AtlasBuildError(f"required SVG asset uses unsupported font style: {relative}")
    elif name in NUMERIC_SVG_ATTRIBUTES:
        validate_svg_css_numeric_syntax(name, value, relative)
    elif name in {"fill", "stroke", "stop-color"}:
        validate_svg_paint_syntax(value, relative)
    elif name in {"filter", "marker-end"}:
        validate_svg_fragment_reference_syntax(value, relative)
    elif name == "stroke-linecap":
        if value not in {"butt", "round", "square"}:
            raise AtlasBuildError(f"required SVG asset uses unsupported stroke linecap: {relative}")
    elif name == "stroke-linejoin":
        if value not in {"bevel", "miter", "round"}:
            raise AtlasBuildError(f"required SVG asset uses unsupported stroke linejoin: {relative}")
    elif name == "text-anchor":
        if value not in {"end", "middle", "start"}:
            raise AtlasBuildError(f"required SVG asset uses unsupported text anchor: {relative}")


def validate_svg_css_stylesheet(css: str, relative: str) -> None:
    guard_resolved_svg_scalar(css, relative, "CSS")
    if not css.isascii():
        raise AtlasBuildError(f"required SVG asset resolves CSS to non-ASCII text: {relative}")
    if (
        len(css) > 65_536
        or "/*" in css
        or "*/" in css
        or "\\" in css
        or "@" in css
        or "<!--" in css
        or "-->" in css
    ):
        raise AtlasBuildError(f"required SVG asset contains unsafe or oversized CSS: {relative}")
    validate_internal_svg_references(css, relative)

    cursor = 0
    rule_count = 0
    while True:
        while cursor < len(css) and css[cursor].isspace():
            cursor += 1
        if cursor == len(css):
            break
        opening = css.find("{", cursor)
        if opening < 0:
            raise AtlasBuildError(f"required SVG asset contains malformed CSS: {relative}")
        closing = css.find("}", opening + 1)
        if closing < 0 or "{" in css[opening + 1 : closing]:
            raise AtlasBuildError(f"required SVG asset contains malformed CSS: {relative}")
        selector = css[cursor:opening].strip()
        class_name = r"\.[A-Za-z_][A-Za-z0-9_-]*"
        if re.fullmatch(rf"{class_name}(?:\s*,\s*{class_name})*", selector) is None:
            raise AtlasBuildError(
                f"required SVG asset contains an unsupported CSS selector: {relative}"
            )

        declarations = css[opening + 1 : closing]
        for declaration in declarations.split(";"):
            declaration = declaration.strip()
            if not declaration:
                continue
            if ":" not in declaration:
                raise AtlasBuildError(f"required SVG asset contains malformed CSS: {relative}")
            raw_name, value = declaration.split(":", 1)
            name = raw_name.strip()
            if re.fullmatch(r"[a-z][a-z-]*", name) is None:
                raise AtlasBuildError(
                    f"required SVG asset contains a non-canonical CSS property: {relative}"
                )
            validate_svg_css_declaration(name, value.strip(), relative)

        rule_count += 1
        if rule_count > MAX_SVG_NODES:
            raise AtlasBuildError(f"required SVG asset exceeds CSS rule limits: {relative}")
        cursor = closing + 1


def normalized_ascii_svg(snapshot: InputSnapshot) -> bytes:
    normalized = snapshot.text
    for character, replacement in SVG_ASCII_REPLACEMENTS.items():
        normalized = normalized.replace(character, replacement)
    residual = sorted({ord(character) for character in normalized if ord(character) >= 0x80})
    if residual:
        labels = ", ".join(f"U+{codepoint:04X}" for codepoint in residual)
        raise AtlasBuildError(
            f"required SVG asset contains unmapped non-ASCII text ({labels}): {snapshot.relative}"
        )
    return normalized.encode("ascii")


class BoundedSvgStructureTarget:
    """Count XML structure without constructing an element tree."""

    def __init__(self, relative: str) -> None:
        self.relative = relative
        self.node_count = 0
        self.depth = 0
        self.attribute_count = 0

    def start(self, _: str, attributes: dict[str, str]) -> None:
        self.node_count += 1
        self.depth += 1
        self.attribute_count += len(attributes)
        if (
            self.node_count > MAX_SVG_NODES
            or self.depth > MAX_SVG_DEPTH
            or self.attribute_count > MAX_SVG_ATTRIBUTES
        ):
            raise AtlasBuildError(
                f"required SVG asset exceeds streaming structural limits: {self.relative}"
            )

    def end(self, _: str) -> None:
        self.depth -= 1
        if self.depth < 0:
            raise AtlasBuildError(f"required SVG asset has malformed depth: {self.relative}")

    def data(self, _: str) -> None:
        return

    def close(self) -> None:
        if self.depth != 0 or self.node_count == 0:
            raise AtlasBuildError(f"required SVG asset has malformed depth: {self.relative}")


def preflight_svg_structure(normalized_data: bytes, relative: str) -> None:
    """Reject excessive node/depth/attribute structure before full-tree parsing."""

    target = BoundedSvgStructureTarget(relative)
    parser = ET.XMLParser(target=target)
    try:
        for offset in range(0, len(normalized_data), SVG_PREFLIGHT_CHUNK_BYTES):
            parser.feed(normalized_data[offset : offset + SVG_PREFLIGHT_CHUNK_BYTES])
        parser.close()
    except AtlasBuildError:
        raise
    except ET.ParseError as error:
        raise AtlasBuildError(f"required SVG asset is malformed: {relative}: {error}") from error


def validate_passive_svg(snapshot: InputSnapshot) -> bytes:
    relative = snapshot.relative
    normalized_data = normalized_ascii_svg(snapshot)
    normalized_text = normalized_data.decode("ascii")
    for character in normalized_text:
        codepoint = ord(character)
        if codepoint < 0x20 and character not in "\t\n\r":
            raise AtlasBuildError(f"required SVG asset contains a control character: {relative}")
    upper = normalized_text.upper()
    if "<!DOCTYPE" in upper or "<!ENTITY" in upper:
        raise AtlasBuildError(f"required SVG asset contains a DTD or entity declaration: {relative}")
    without_xml_declaration = re.sub(
        r"^\s*<\?xml\s+[^?]*\?>",
        "",
        normalized_text,
        count=1,
        flags=re.IGNORECASE,
    )
    if "<?" in without_xml_declaration:
        raise AtlasBuildError(f"required SVG asset contains a processing instruction: {relative}")

    preflight_svg_structure(normalized_data, relative)
    try:
        root = ET.fromstring(normalized_data)
    except ET.ParseError as error:
        raise AtlasBuildError(f"required SVG asset is malformed: {relative}: {error}") from error
    if xml_local_name(root.tag) != "svg":
        raise AtlasBuildError(f"required SVG asset root is not <svg>: {relative}")

    view_box = next(
        (value for name, value in root.attrib.items() if xml_local_name(name) == "viewBox"),
        None,
    )
    if view_box is None:
        raise AtlasBuildError(f"required SVG asset has no viewBox: {relative}")
    try:
        view_values = [float(value) for value in re.split(r"[\s,]+", view_box.strip())]
    except ValueError as error:
        raise AtlasBuildError(f"required SVG asset has a non-numeric viewBox: {relative}") from error
    if (
        len(view_values) != 4
        or not all(math.isfinite(value) for value in view_values)
        or view_values[2] <= 0
        or view_values[3] <= 0
        or any(abs(value) > 10_000_000 for value in view_values)
    ):
        raise AtlasBuildError(f"required SVG asset has an invalid viewBox: {relative}")

    node_count = 0
    attribute_count = 0
    path_characters = 0
    stack: list[tuple[ET.Element, int]] = [(root, 1)]
    while stack:
        element, depth = stack.pop()
        node_count += 1
        if node_count > MAX_SVG_NODES or depth > MAX_SVG_DEPTH:
            raise AtlasBuildError(f"required SVG asset exceeds structural limits: {relative}")
        tag = xml_local_name(element.tag)
        if tag not in SAFE_SVG_TAGS:
            raise AtlasBuildError(f"required SVG asset contains unsupported <{tag}> content: {relative}")
        for text_context, resolved_text in (
            ("element text", element.text or ""),
            ("element tail", element.tail or ""),
        ):
            guard_resolved_svg_scalar(resolved_text, relative, text_context)
            if not resolved_text.isascii():
                raise AtlasBuildError(
                    f"required SVG asset resolves an entity to non-ASCII text: {relative}"
                )
        if tag == "style":
            css = element.text or ""
            validate_svg_css_stylesheet(css, relative)
        attribute_count += len(element.attrib)
        if attribute_count > MAX_SVG_ATTRIBUTES:
            raise AtlasBuildError(f"required SVG asset has too many attributes: {relative}")
        for name, value in element.attrib.items():
            local_name = xml_local_name(name)
            guard_resolved_svg_scalar(value, relative, f"attribute {local_name}")
            if not value.isascii():
                raise AtlasBuildError(
                    f"required SVG asset resolves an attribute entity to non-ASCII text: {relative}"
                )
            validate_svg_environment_free_value(value, relative)
            if local_name not in SAFE_SVG_ATTRIBUTES:
                raise AtlasBuildError(
                    f"required SVG asset contains unsupported attribute {local_name!r}: {relative}"
                )
            if local_name.lower().startswith("on"):
                raise AtlasBuildError(f"required SVG asset contains an event handler: {relative}")
            if local_name.lower() in {"href", "src"} and value and not value.startswith("#"):
                raise AtlasBuildError(
                    f"required SVG asset contains an external reference: {relative}"
                )
            validate_internal_svg_references(value, relative)
            if local_name in {"fill", "stroke", "stop-color"}:
                validate_svg_paint_syntax(value, relative)
            elif local_name in {"filter", "marker-end"}:
                validate_svg_fragment_reference_syntax(value, relative)
            if re.search(r"(?i)(?:^|[^a-z])(?:nan|[+-]?inf(?:inity)?)(?:$|[^a-z])", value):
                raise AtlasBuildError(f"required SVG asset contains a non-finite number: {relative}")
            if local_name == "font-family":
                validate_canonical_svg_font_family(value, relative)
            elif local_name == "font-weight":
                validate_svg_font_weight(value, relative)
            if local_name in NUMERIC_SVG_ATTRIBUTES:
                validate_svg_numeric_value(local_name, value, relative)
            if tag == "path" and local_name == "d":
                path_characters += len(value)
                if path_characters > MAX_SVG_PATH_CHARACTERS:
                    raise AtlasBuildError(f"required SVG asset path data is too large: {relative}")
        stack.extend((child, depth + 1) for child in list(element))
    return normalized_data


def validate_inputs(
    root: Path, bound_builder_bytes: bytes | None = None
) -> tuple[dict[str, InputSnapshot], str]:
    builder_snapshot = bound_builder_snapshot(root, bound_builder_bytes)
    declared = (*REQUIRED_ASSETS, *SOURCE_CONTRACTS.keys())
    if len(declared) != EXPECTED_INPUT_COUNT or len(set(declared)) != EXPECTED_INPUT_COUNT:
        raise AtlasBuildError(
            f"atlas input declaration must contain {EXPECTED_INPUT_COUNT} unique paths"
        )
    if declared.count(BUILDER_SOURCE) != 1:
        raise AtlasBuildError("atlas builder source must appear exactly once in the input identity")

    files: dict[str, InputSnapshot] = {}
    fingerprint_records: list[str] = []
    total_bytes = 0

    for relative in REQUIRED_ASSETS:
        snapshot = read_input_snapshot(root, relative, "SVG asset")
        validate_passive_svg(snapshot)
        files[relative] = snapshot
        total_bytes += snapshot.size
        if total_bytes > MAX_TOTAL_SOURCE_BYTES:
            raise AtlasBuildError(
                f"atlas inputs exceed the {MAX_TOTAL_SOURCE_BYTES}-byte aggregate cap"
            )
        fingerprint_records.append(
            f"{relative}\0{snapshot.size}\0{snapshot.sha256}"
        )

    for relative, markers in SOURCE_CONTRACTS.items():
        snapshot = (
            builder_snapshot
            if relative == BUILDER_SOURCE
            else read_input_snapshot(root, relative, "source contract")
        )
        absent = [marker for marker in markers if marker not in snapshot.text]
        if absent:
            raise AtlasBuildError(
                f"source contract no longer matches the atlas boundary: {relative}; "
                f"missing markers {absent}"
            )
        files[relative] = snapshot
        total_bytes += snapshot.size
        if total_bytes > MAX_TOTAL_SOURCE_BYTES:
            raise AtlasBuildError(
                f"atlas inputs exceed the {MAX_TOTAL_SOURCE_BYTES}-byte aggregate cap"
            )
        fingerprint_records.append(
            f"{relative}\0{snapshot.size}\0{snapshot.sha256}"
        )

    if total_bytes > MAX_TOTAL_SOURCE_BYTES:
        raise AtlasBuildError(
            f"atlas inputs exceed the {MAX_TOTAL_SOURCE_BYTES}-byte aggregate cap"
        )
    if len(files) != EXPECTED_INPUT_COUNT or len(fingerprint_records) != EXPECTED_INPUT_COUNT:
        raise AtlasBuildError("atlas input identity is incomplete")

    payload = "\n".join(sorted(fingerprint_records)).encode("utf-8")
    fingerprint = hashlib.sha256(payload).hexdigest()
    return files, fingerprint


def validate_portable_output_components(root: Path, output: Path) -> None:
    reserved = {
        "CON",
        "PRN",
        "AUX",
        "NUL",
        *(f"COM{index}" for index in range(1, 10)),
        *(f"LPT{index}" for index in range(1, 10)),
    }
    for component in output.relative_to(root).parts:
        if (
            not component
            or component[-1] in " ."
            or any(ord(character) < 0x20 or character in '<>:"|?*' for character in component)
            or component.split(".", 1)[0].upper() in reserved
        ):
            raise AtlasBuildError(
                f"PDF output contains an unsafe or non-portable path component: {component!r}"
            )


def validate_output(
    root: Path, raw_output: str, force: bool, *, check_only: bool = False
) -> OutputTarget:
    candidate = Path(raw_output)
    if not candidate.is_absolute():
        candidate = root / candidate
    output = lexical_absolute(candidate)
    if output.suffix.lower() != ".pdf":
        raise AtlasBuildError("--output must end in .pdf")

    allowed_roots = (
        lexical_absolute(root / "output" / "pdf"),
        lexical_absolute(root / "tmp"),
    )
    allowed_root = next(
        (directory for directory in allowed_roots if is_relative_to(output, directory)),
        None,
    )
    if allowed_root is None:
        raise AtlasBuildError("PDF output must stay under repository output/pdf or tmp")
    validate_portable_output_components(root, output)
    assert_no_reparse_components(
        output,
        root,
        allow_missing=True,
        label="PDF output",
    )
    resolved_output = output.resolve(strict=False)
    resolved_allowed_root = allowed_root.resolve(strict=False)
    if not is_relative_to(resolved_output, resolved_allowed_root):
        raise AtlasBuildError("PDF output resolves outside its allowed root")

    output_info = lstat_or_none(output)
    if output_info is not None and is_reparse_stat(output_info):
        raise AtlasBuildError(f"output is a symlink or reparse point: {output}")
    if output_info is not None and not stat.S_ISREG(output_info.st_mode):
        raise AtlasBuildError(f"output is not a regular file: {output}")
    if output_info is not None and not force and not check_only:
        raise AtlasBuildError(
            f"output already exists (default is no-clobber): {output}; pass --force to replace it"
        )
    return OutputTarget(path=output, allowed_root=allowed_root)


def canonical_toolchain_version(label: str, value: object) -> str:
    text = str(value).strip()
    if (
        not text
        or text.lower() == "none"
        or "unknown" in text.lower()
        or re.fullmatch(r"[0-9A-Za-z][0-9A-Za-z.+_-]*", text) is None
    ):
        raise AtlasBuildError(f"PDF dependency {label} has no canonical version identity")
    return text


def canonical_compiled_version(label: str, value: object) -> str:
    if (
        not isinstance(value, tuple)
        or not value
        or any(type(component) is not int or component < 0 for component in value)
    ):
        raise AtlasBuildError(f"PDF dependency {label} has no canonical compiled identity")
    return ".".join(str(component) for component in value)


def load_pdf_dependencies() -> dict[str, Any]:
    try:
        import cssselect2 as cssselect2_module
        import lxml as lxml_module
        import pypdf as pypdf_module
        import reportlab as reportlab_module
        import svglib as svglib_module
        import tinycss2 as tinycss2_module
        import zlib as zlib_module
        from lxml import etree as lxml_etree
        from pypdf import PdfReader
        from pypdf.generic import TextStringObject
        from reportlab.graphics.shapes import Drawing
        from reportlab.lib import colors
        from reportlab.lib.enums import TA_CENTER, TA_LEFT
        from reportlab.lib.pagesizes import A4
        from reportlab.lib.styles import ParagraphStyle, getSampleStyleSheet
        from reportlab.lib.units import mm
        from reportlab.pdfgen import canvas
        from reportlab.platypus import (
            BaseDocTemplate,
            Flowable,
            Frame,
            HRFlowable,
            KeepTogether,
            PageBreak,
            PageTemplate,
            Paragraph,
            Spacer,
            Table,
            TableStyle,
        )
        from svglib.svglib import svg2rlg
    except ImportError as error:
        raise AtlasBuildError(
            "PDF dependencies are unavailable. Install reportlab, svglib, pypdf, lxml, "
            "cssselect2, and tinycss2 or use the bundled Codex document runtime."
        ) from error

    toolchain_components = {
        "cssselect2": canonical_toolchain_version(
            "cssselect2", getattr(cssselect2_module, "__version__", None)
        ),
        "libxml2-compiled": canonical_compiled_version(
            "compiled libxml2", getattr(lxml_etree, "LIBXML_COMPILED_VERSION", None)
        ),
        "libxml2-runtime": canonical_compiled_version(
            "runtime libxml2", getattr(lxml_etree, "LIBXML_VERSION", None)
        ),
        "libxslt-compiled": canonical_compiled_version(
            "compiled libxslt", getattr(lxml_etree, "LIBXSLT_COMPILED_VERSION", None)
        ),
        "libxslt-runtime": canonical_compiled_version(
            "runtime libxslt", getattr(lxml_etree, "LIBXSLT_VERSION", None)
        ),
        "lxml": canonical_toolchain_version(
            "lxml", getattr(lxml_module, "__version__", None)
        ),
        "pypdf": canonical_toolchain_version(
            "pypdf", getattr(pypdf_module, "__version__", None)
        ),
        "python": canonical_toolchain_version(
            "python",
            f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}",
        ),
        "reportlab": canonical_toolchain_version(
            "reportlab", getattr(reportlab_module, "Version", None)
        ),
        "svglib": canonical_toolchain_version(
            "svglib", getattr(svglib_module, "__version__", None)
        ),
        "tinycss2": canonical_toolchain_version(
            "tinycss2", getattr(tinycss2_module, "__version__", None)
        ),
        "zlib-compiled": canonical_toolchain_version(
            "compiled zlib", getattr(zlib_module, "ZLIB_VERSION", None)
        ),
        "zlib-runtime": canonical_toolchain_version(
            "runtime zlib", getattr(zlib_module, "ZLIB_RUNTIME_VERSION", None)
        ),
    }
    toolchain_identity = ";".join(
        f"{name}={toolchain_components[name]}" for name in sorted(toolchain_components)
    )
    return locals()


def guard_public_text(value: object) -> str:
    text = str(value)
    absolute_path_patterns = (
        r"(?i)(?<![a-z0-9])[a-z]:[\\/]",
        r"(?i)(?<![:a-z0-9])(?:\\\\|//)[^\\/\s]+[\\/]",
        r"(?i)(?<![a-z0-9])/(?:applications|bin|data|dev|etc|home|lib|mnt|opt|private|proc|root|run|srv|system|tmp|users|usr|var|volumes|workspace)(?:/|$)",
    )
    if any(re.search(pattern, text) for pattern in absolute_path_patterns):
        raise AtlasBuildError("refusing to embed an absolute workstation path in the PDF")
    for character in text:
        codepoint = ord(character)
        if (codepoint < 0x20 and character not in "\t\n\r") or 0x7F <= codepoint <= 0x9F:
            raise AtlasBuildError("public atlas text contains a forbidden control character")
        if (
            codepoint == 0xFFFD
            or 0xFDD0 <= codepoint <= 0xFDEF
            or codepoint & 0xFFFF in {0xFFFE, 0xFFFF}
            or 0xD800 <= codepoint <= 0xDFFF
        ):
            raise AtlasBuildError("public atlas text contains a replacement or noncharacter code point")
        if codepoint in {0x25A1, 0x25AF}:
            raise AtlasBuildError("public atlas text contains a replacement-box glyph")
        if character in UNICODE_DASHES:
            raise AtlasBuildError(
                "public atlas text contains a Unicode dash; use the ASCII hyphen instead"
            )
    return text


def parse_svg_drawing(snapshot: InputSnapshot, svg2rlg: Any) -> Any:
    normalized_data = validate_passive_svg(snapshot)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        try:
            drawing = svg2rlg(io.BytesIO(normalized_data))
        except Exception as error:  # svglib provides several backend exception types
            raise AtlasBuildError(
                f"could not parse required SVG asset {snapshot.relative}: {error}"
            ) from error
    if caught:
        messages = "; ".join(str(item.message) for item in caught[:3])
        raise AtlasBuildError(
            f"required SVG asset emitted converter warnings: {snapshot.relative}: {messages}"
        )
    if drawing is None:
        raise AtlasBuildError(f"required SVG asset produced no drawing: {snapshot.relative}")
    width = float(drawing.width)
    height = float(drawing.height)
    if not math.isfinite(width) or not math.isfinite(height) or width <= 0 or height <= 0:
        raise AtlasBuildError(f"required SVG asset has invalid geometry: {snapshot.relative}")
    return drawing


def validate_svg_snapshots(files: dict[str, InputSnapshot], svg2rlg: Any) -> None:
    for relative in REQUIRED_ASSETS:
        parse_svg_drawing(files[relative], svg2rlg)


class BoundedBytesIO(io.BytesIO):
    def __init__(self, limit: int) -> None:
        super().__init__()
        self.limit = limit

    def write(self, data: bytes) -> int:
        if self.tell() + len(data) > self.limit:
            raise AtlasBuildError(f"generated PDF exceeds the {self.limit}-byte output cap")
        return super().write(data)


def strip_implicit_reportlab_page_interaction(page: Any) -> None:
    """Remove ReportLab's empty transition dictionary and any page duration."""

    state = vars(page)
    state.pop("Trans", None)
    state.pop("Dur", None)


def compute_atlas_document_identity(
    files: dict[str, InputSnapshot],
    fingerprint: str,
    toolchain_identity: str,
) -> AtlasDocumentIdentity:
    builder_snapshot = files.get(BUILDER_SOURCE)
    if not isinstance(builder_snapshot, InputSnapshot):
        raise AtlasBuildError("atlas document identity is missing its builder snapshot")
    if re.fullmatch(r"[0-9a-f]{64}", fingerprint) is None:
        raise AtlasBuildError("atlas document identity has a non-canonical source fingerprint")
    if re.fullmatch(r"[0-9a-f]{64}", builder_snapshot.sha256) is None:
        raise AtlasBuildError("atlas document identity has a non-canonical builder SHA-256")
    if not isinstance(toolchain_identity, str) or not toolchain_identity:
        raise AtlasBuildError("atlas document identity has no toolchain identity")

    document_id_hex = hashlib.sha256(
        (
            "voxel-native-codex-engineering-atlas-v1\0"
            + fingerprint
            + "\0"
            + builder_snapshot.sha256
            + "\0"
            + toolchain_identity
        ).encode("utf-8")
    ).hexdigest()[:32].upper()
    return AtlasDocumentIdentity(
        fingerprint=fingerprint,
        builder_sha=builder_snapshot.sha256,
        toolchain_identity=toolchain_identity,
        document_id_hex=document_id_hex,
    )


def build_pdf_bytes(
    files: dict[str, InputSnapshot],
    fingerprint: str,
    dep: dict[str, Any],
) -> BuiltPdf:
    PdfReader = dep["PdfReader"]
    TextStringObject = dep["TextStringObject"]
    colors = dep["colors"]
    TA_CENTER = dep["TA_CENTER"]
    TA_LEFT = dep["TA_LEFT"]
    A4 = dep["A4"]
    ParagraphStyle = dep["ParagraphStyle"]
    getSampleStyleSheet = dep["getSampleStyleSheet"]
    mm = dep["mm"]
    canvas_module = dep["canvas"]
    BaseDocTemplate = dep["BaseDocTemplate"]
    Frame = dep["Frame"]
    HRFlowable = dep["HRFlowable"]
    KeepTogether = dep["KeepTogether"]
    PageBreak = dep["PageBreak"]
    PageTemplate = dep["PageTemplate"]
    Paragraph = dep["Paragraph"]
    Spacer = dep["Spacer"]
    Table = dep["Table"]
    TableStyle = dep["TableStyle"]
    svg2rlg = dep["svg2rlg"]

    page_w, page_h = A4
    left = right = 18 * mm
    top = 18 * mm
    bottom = 17 * mm
    content_w = page_w - left - right
    identity = compute_atlas_document_identity(
        files,
        fingerprint,
        dep["toolchain_identity"],
    )
    builder_sha = identity.builder_sha
    document_id_hex = identity.document_id_hex
    output_buffer = BoundedBytesIO(MAX_PDF_BYTES)

    ink = colors.HexColor("#10272F")
    navy = colors.HexColor("#0B2B36")
    deep = colors.HexColor("#07161C")
    teal = colors.HexColor("#0C9B7B")
    cyan = colors.HexColor("#238EC2")
    lime = colors.HexColor("#A9D858")
    amber = colors.HexColor("#E6A647")
    coral = colors.HexColor("#DA6572")
    paper = colors.HexColor("#F4F7F5")
    white = colors.white
    pale = colors.HexColor("#E6F1EE")
    pale_blue = colors.HexColor("#E5EFF5")
    pale_amber = colors.HexColor("#FFF0D5")
    pale_coral = colors.HexColor("#FBE7EA")
    line = colors.HexColor("#C4D5D2")
    muted = colors.HexColor("#526A72")
    soft = colors.HexColor("#EEF3F1")

    base = getSampleStyleSheet()
    styles: dict[str, Any] = {
        "cover_kicker": ParagraphStyle(
            "CoverKicker",
            parent=base["Normal"],
            fontName="Helvetica-Bold",
            fontSize=9,
            leading=11,
            textColor=colors.HexColor("#70F8D8"),
            spaceAfter=3 * mm,
        ),
        "cover_title": ParagraphStyle(
            "CoverTitle",
            parent=base["Title"],
            fontName="Helvetica-Bold",
            fontSize=27,
            leading=30,
            textColor=white,
            spaceAfter=3 * mm,
        ),
        "cover_deck": ParagraphStyle(
            "CoverDeck",
            parent=base["Normal"],
            fontName="Helvetica",
            fontSize=11,
            leading=15,
            textColor=colors.HexColor("#B8CCD3"),
            spaceAfter=4 * mm,
        ),
        "kicker": ParagraphStyle(
            "Kicker",
            parent=base["Normal"],
            fontName="Helvetica-Bold",
            fontSize=7.4,
            leading=9,
            textColor=teal,
            spaceAfter=1.4 * mm,
        ),
        "title": ParagraphStyle(
            "PageTitle",
            parent=base["Heading1"],
            fontName="Helvetica-Bold",
            fontSize=19,
            leading=22,
            textColor=navy,
            spaceAfter=2 * mm,
            keepWithNext=True,
        ),
        "deck": ParagraphStyle(
            "Deck",
            parent=base["Normal"],
            fontName="Helvetica",
            fontSize=9.5,
            leading=13,
            textColor=muted,
            spaceAfter=3 * mm,
        ),
        "h2": ParagraphStyle(
            "Heading2",
            parent=base["Heading2"],
            fontName="Helvetica-Bold",
            fontSize=11.5,
            leading=14,
            textColor=navy,
            spaceBefore=2 * mm,
            spaceAfter=1.5 * mm,
            keepWithNext=True,
        ),
        "body": ParagraphStyle(
            "Body",
            parent=base["BodyText"],
            fontName="Helvetica",
            fontSize=8.7,
            leading=12.1,
            textColor=ink,
            spaceAfter=2 * mm,
        ),
        "small": ParagraphStyle(
            "Small",
            parent=base["BodyText"],
            fontName="Helvetica",
            fontSize=7.1,
            leading=9.4,
            textColor=muted,
            spaceAfter=1.3 * mm,
        ),
        "caption": ParagraphStyle(
            "Caption",
            parent=base["BodyText"],
            fontName="Helvetica",
            fontSize=6.8,
            leading=8.7,
            textColor=muted,
            alignment=TA_CENTER,
            spaceBefore=1 * mm,
            spaceAfter=2 * mm,
        ),
        "table_head": ParagraphStyle(
            "TableHead",
            parent=base["Normal"],
            fontName="Helvetica-Bold",
            fontSize=6.8,
            leading=8.2,
            textColor=white,
        ),
        "table": ParagraphStyle(
            "TableBody",
            parent=base["Normal"],
            fontName="Helvetica",
            fontSize=6.8,
            leading=8.5,
            textColor=ink,
        ),
        "table_bold": ParagraphStyle(
            "TableBold",
            parent=base["Normal"],
            fontName="Helvetica-Bold",
            fontSize=6.8,
            leading=8.5,
            textColor=ink,
        ),
        "formula_label": ParagraphStyle(
            "FormulaLabel",
            parent=base["Normal"],
            fontName="Helvetica-Bold",
            fontSize=6.7,
            leading=8,
            textColor=teal,
            spaceAfter=1.2 * mm,
        ),
        "formula": ParagraphStyle(
            "Formula",
            parent=base["Code"],
            fontName="Courier-Bold",
            fontSize=9.3,
            leading=12,
            textColor=navy,
            spaceAfter=1.5 * mm,
        ),
        "formula_small": ParagraphStyle(
            "FormulaSmall",
            parent=base["Code"],
            fontName="Courier-Bold",
            fontSize=7.4,
            leading=9.5,
            textColor=navy,
            spaceAfter=1.2 * mm,
        ),
        "callout_label": ParagraphStyle(
            "CalloutLabel",
            parent=base["Normal"],
            fontName="Helvetica-Bold",
            fontSize=7.2,
            leading=9,
            textColor=navy,
            spaceAfter=1 * mm,
        ),
        "callout": ParagraphStyle(
            "Callout",
            parent=base["Normal"],
            fontName="Helvetica",
            fontSize=8,
            leading=11,
            textColor=ink,
        ),
        "bullet": ParagraphStyle(
            "Bullet",
            parent=base["BodyText"],
            fontName="Helvetica",
            fontSize=8,
            leading=10.8,
            leftIndent=4 * mm,
            firstLineIndent=-3 * mm,
            textColor=ink,
            spaceAfter=1.3 * mm,
        ),
        "step": ParagraphStyle(
            "Step",
            parent=base["Normal"],
            fontName="Helvetica-Bold",
            fontSize=8.2,
            leading=10.5,
            textColor=navy,
        ),
        "reference": ParagraphStyle(
            "Reference",
            parent=base["BodyText"],
            fontName="Helvetica",
            fontSize=6.5,
            leading=8.2,
            textColor=ink,
        ),
    }

    def paragraph(
        text: object,
        style: str = "body",
        *,
        preserve_line_breaks: bool = False,
    ) -> Any:
        public = guard_public_text(text)
        if preserve_line_breaks:
            escaped_lines: list[str] = []
            for line_text in public.split("\n"):
                leading_spaces = len(line_text) - len(line_text.lstrip(" "))
                escaped_lines.append(
                    "&#160;" * leading_spaces + html.escape(line_text[leading_spaces:])
                )
            escaped = "<br/>".join(escaped_lines)
        else:
            escaped = html.escape(public)
        return Paragraph(escaped, styles[style])

    def markup(text: str, style: str = "body") -> Any:
        guard_public_text(re.sub(r"<[^>]+>", "", text))
        return Paragraph(text, styles[style])

    def bullet(text: str) -> Any:
        return paragraph(f"- {text}", "bullet")

    def section_header(kicker: str, title: str, deck: str) -> list[Any]:
        return [
            paragraph(kicker.upper(), "kicker"),
            paragraph(title, "title"),
            paragraph(deck, "deck"),
            HRFlowable(width="100%", thickness=0.8, color=line, spaceAfter=3 * mm),
        ]

    def source_note(paths: Sequence[str]) -> Any:
        return paragraph("Source boundary: " + " | ".join(paths), "small")

    def matrix(
        headers: Sequence[str],
        rows: Sequence[Sequence[object]],
        widths_mm: Sequence[float],
        *,
        body_style: str = "table",
        first_column_bold: bool = True,
        font_size_padding: float = 4,
    ) -> Any:
        if len(headers) != len(widths_mm) or abs(sum(widths_mm) - 174.0) > 0.01:
            raise AtlasBuildError("table widths must fill the 174 mm content frame")
        data: list[list[Any]] = [
            [paragraph(header, "table_head") for header in headers]
        ]
        for row in rows:
            if len(row) != len(headers):
                raise AtlasBuildError("table row does not match its header")
            cells: list[Any] = []
            for index, value in enumerate(row):
                style = "table_bold" if first_column_bold and index == 0 else body_style
                cells.append(paragraph(value, style))
            data.append(cells)
        table = Table(
            data,
            colWidths=[value * mm for value in widths_mm],
            repeatRows=1,
            hAlign="LEFT",
            splitByRow=1,
        )
        commands: list[tuple[Any, ...]] = [
            ("BACKGROUND", (0, 0), (-1, 0), navy),
            ("VALIGN", (0, 0), (-1, -1), "TOP"),
            ("LEFTPADDING", (0, 0), (-1, -1), font_size_padding),
            ("RIGHTPADDING", (0, 0), (-1, -1), font_size_padding),
            ("TOPPADDING", (0, 0), (-1, -1), font_size_padding),
            ("BOTTOMPADDING", (0, 0), (-1, -1), font_size_padding),
            ("GRID", (0, 0), (-1, -1), 0.35, line),
        ]
        for row_index in range(1, len(data)):
            commands.append(
                (
                    "BACKGROUND",
                    (0, row_index),
                    (-1, row_index),
                    white if row_index % 2 else soft,
                )
            )
        table.setStyle(TableStyle(commands))
        return table

    def callout(
        label: str, text: str, tone: str = "teal", *, width_mm: float = 174
    ) -> Any:
        palette = {
            "teal": (pale, teal),
            "blue": (pale_blue, cyan),
            "amber": (pale_amber, amber),
            "coral": (pale_coral, coral),
        }
        background, accent = palette[tone]
        label_width = 36 if width_mm >= 150 else 27
        table = Table(
            [[paragraph(label.upper(), "callout_label"), paragraph(text, "callout")]],
            colWidths=[label_width * mm, (width_mm - label_width) * mm],
            hAlign="LEFT",
        )
        table.setStyle(
            TableStyle(
                [
                    ("BACKGROUND", (0, 0), (-1, -1), background),
                    ("LINEBEFORE", (0, 0), (0, -1), 3, accent),
                    ("VALIGN", (0, 0), (-1, -1), "TOP"),
                    ("LEFTPADDING", (0, 0), (-1, -1), 7),
                    ("RIGHTPADDING", (0, 0), (-1, -1), 7),
                    ("TOPPADDING", (0, 0), (-1, -1), 7),
                    ("BOTTOMPADDING", (0, 0), (-1, -1), 7),
                ]
            )
        )
        return table

    def formula_card(
        label: str,
        formula: str,
        note: str,
        *,
        compact: bool = False,
        width_mm: float = 84,
    ) -> Any:
        formula_style = "formula_small" if compact else "formula"
        data = [
            [paragraph(label.upper(), "formula_label")],
            [paragraph(formula, formula_style, preserve_line_breaks=True)],
            [paragraph(note, "small")],
        ]
        table = Table(data, colWidths=[width_mm * mm], hAlign="LEFT")
        table.setStyle(
            TableStyle(
                [
                    ("BACKGROUND", (0, 0), (-1, -1), white),
                    ("BOX", (0, 0), (-1, -1), 0.7, line),
                    ("LINEABOVE", (0, 0), (-1, 0), 2.2, teal),
                    ("LEFTPADDING", (0, 0), (-1, -1), 8),
                    ("RIGHTPADDING", (0, 0), (-1, -1), 8),
                    ("TOPPADDING", (0, 0), (-1, -1), 6),
                    ("BOTTOMPADDING", (0, 0), (-1, -1), 6),
                ]
            )
        )
        return table

    def two_columns(left_items: Sequence[Any], right_items: Sequence[Any]) -> Any:
        table = Table(
            [[list(left_items), list(right_items)]],
            colWidths=[85 * mm, 85 * mm],
            hAlign="LEFT",
        )
        table.setStyle(
            TableStyle(
                [
                    ("VALIGN", (0, 0), (-1, -1), "TOP"),
                    ("LEFTPADDING", (0, 0), (0, 0), 0),
                    ("RIGHTPADDING", (0, 0), (0, 0), 4),
                    ("LEFTPADDING", (1, 0), (1, 0), 4),
                    ("RIGHTPADDING", (1, 0), (1, 0), 0),
                    ("TOPPADDING", (0, 0), (-1, -1), 0),
                    ("BOTTOMPADDING", (0, 0), (-1, -1), 0),
                ]
            )
        )
        return table

    def svg_asset(relative: str, max_width_mm: float, max_height_mm: float) -> Any:
        drawing = parse_svg_drawing(files[relative], svg2rlg)
        scale = min(
            (max_width_mm * mm) / drawing.width,
            (max_height_mm * mm) / drawing.height,
        )
        if not math.isfinite(scale) or scale <= 0:
            raise AtlasBuildError(f"required SVG asset has an invalid scale: {relative}")
        drawing.scale(scale, scale)
        drawing.width *= scale
        drawing.height *= scale
        drawing.hAlign = "CENTER"
        return drawing

    def reference_paragraph(label: str, purpose: str, url: str) -> Any:
        guard_public_text(label)
        guard_public_text(purpose)
        guard_public_text(url)
        if url not in OFFICIAL_URIS or not url.startswith("https://"):
            raise AtlasBuildError(f"reference URL is outside the fixed HTTPS allowlist: {url}")
        return Paragraph(
            f"<b>{html.escape(label)}</b><br/>{html.escape(purpose)}<br/>"
            f"<link href=\"{html.escape(url, quote=True)}\" color=\"#238EC2\">"
            f"{html.escape(url)}</link>",
            styles["reference"],
        )

    class InvariantCanvas(canvas_module.Canvas):
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            kwargs["invariant"] = 1
            kwargs["pageCompression"] = 1
            super().__init__(*args, **kwargs)
            self.setTitle(PDF_METADATA["title"])
            self.setAuthor(PDF_METADATA["author"])
            self.setCreator(PDF_METADATA["creator"])
            self.setSubject(PDF_METADATA["subject"])
            self.setKeywords(PDF_METADATA["keywords"])
            self.setDateFormatter(lambda *_: CANONICAL_PDF_DATE)
            self._doc.info.producer = PDF_METADATA["producer"]
            self._doc._ID = (
                f"\n[<{document_id_hex}><{document_id_hex}>]\n".encode("ascii")
            )

        def showPage(self) -> None:
            super().showPage()
            strip_implicit_reportlab_page_interaction(self._doc.Pages.pages[-1])

    def draw_page(canvas: Any, document: Any) -> None:
        page_number = canvas.getPageNumber()
        canvas.saveState()
        if page_number == 1:
            canvas.setFillColor(deep)
            canvas.rect(0, 0, page_w, page_h, fill=1, stroke=0)
            canvas.setStrokeColor(colors.HexColor("#28505A"))
            canvas.line(left, 10 * mm, page_w - right, 10 * mm)
            canvas.setFillColor(colors.HexColor("#93AFB8"))
            canvas.setFont("Helvetica", 6.5)
            canvas.drawString(left, 6.7 * mm, "PROJECT-AUTHORED TECHNICAL ATLAS")
            canvas.drawRightString(
                page_w - right,
                6.7 * mm,
                "NO RUNTIME RELEASE VERDICT / RUNTIME GALLERY PENDING",
            )
        else:
            canvas.setFillColor(paper)
            canvas.rect(0, 0, page_w, page_h, fill=1, stroke=0)
            canvas.setFillColor(teal)
            canvas.rect(0, page_h - 3.2 * mm, page_w, 3.2 * mm, fill=1, stroke=0)
            canvas.setFillColor(muted)
            canvas.setFont("Helvetica-Bold", 6.2)
            canvas.drawString(left, page_h - 10.2 * mm, "VOXEL NATIVE / CODEX ENGINEERING ATLAS")
            canvas.setFillColor(coral)
            canvas.drawRightString(page_w - right, page_h - 10.2 * mm, "RUNTIME GALLERY PENDING")
            canvas.setStrokeColor(line)
            canvas.line(left, 12 * mm, page_w - right, 12 * mm)
            canvas.setFillColor(muted)
            canvas.setFont("Helvetica", 6.3)
            canvas.drawString(left, 7.6 * mm, "SOURCE-FIRST / BOUNDED / REVERSIBLE")
            canvas.drawRightString(page_w - right, 7.6 * mm, f"{page_number:02d} / {EXPECTED_PAGE_COUNT:02d}")
        canvas.restoreState()

    document = BaseDocTemplate(
        output_buffer,
        pagesize=A4,
        leftMargin=left,
        rightMargin=right,
        topMargin=top,
        bottomMargin=bottom,
        title=PDF_METADATA["title"],
        author=PDF_METADATA["author"],
        subject=PDF_METADATA["subject"],
        creator=PDF_METADATA["creator"],
        producer=PDF_METADATA["producer"],
        keywords=PDF_METADATA["keywords"],
        pageCompression=1,
    )
    frame = Frame(
        left,
        bottom,
        content_w,
        page_h - top - bottom,
        id="atlas-frame",
        leftPadding=0,
        rightPadding=0,
        topPadding=0,
        bottomPadding=0,
    )
    document.addPageTemplates([PageTemplate(id="atlas", frames=[frame], onPage=draw_page)])

    story: list[Any] = []

    # 01 - Cover
    story.extend(
        [
            Spacer(1, 1 * mm),
            svg_asset("docs/media/voxel-native-hero.svg", 174, 78),
            markup(
                "<font color=\"#93AFB8\">Project diagram: docs/media/voxel-native-hero.svg.</font>",
                "caption",
            ),
            Spacer(1, 4 * mm),
            paragraph("PROJECT-AUTHORED / FORMULA-FIRST / CONTRACT-BOUND", "cover_kicker"),
            paragraph("CODEX ENGINEERING ATLAS", "cover_title"),
            paragraph(
                "A visual map of the mathematics, representation boundaries, fixed budgets, failure behavior, and evidence discipline behind Voxel Native.",
                "cover_deck",
            ),
        ]
    )
    cover_callout = Table(
        [[
            markup(
                "<font color=\"#70F8D8\"><b>NO RUNTIME RELEASE VERDICT</b></font>",
                "callout_label",
            ),
            markup(
                "<font color=\"#D1E1E5\">Runtime gallery pending. This atlas explains current source contracts; it does not convert a generated image, test process, or benchmark into visual acceptance.</font>",
                "callout",
            ),
        ]],
        colWidths=[47 * mm, 127 * mm],
    )
    cover_callout.setStyle(
        TableStyle(
            [
                ("BACKGROUND", (0, 0), (-1, -1), colors.HexColor("#102A33")),
                ("BOX", (0, 0), (-1, -1), 0.8, colors.HexColor("#3C6670")),
                ("VALIGN", (0, 0), (-1, -1), "TOP"),
                ("LEFTPADDING", (0, 0), (-1, -1), 8),
                ("RIGHTPADDING", (0, 0), (-1, -1), 8),
                ("TOPPADDING", (0, 0), (-1, -1), 8),
                ("BOTTOMPADDING", (0, 0), (-1, -1), 8),
                ("TEXTCOLOR", (0, 0), (-1, -1), white),
            ]
        )
    )
    story.extend(
        [
            cover_callout,
            Spacer(1, 5 * mm),
            markup(
                f"<font color=\"#93AFB8\">INPUT FINGERPRINT / {fingerprint[:24]}<br/>"
                f"BUILDER SHA-256 / {builder_sha[:24]}</font>",
                "small",
            ),
            PageBreak(),
        ]
    )

    # 02 - Status vocabulary
    story.extend(
        section_header(
            "01 / HONEST STATUS VOCABULARY",
            "A technical map, not a cinematic promise",
            "Status words name integration boundaries. They never stand in for a release verdict or a completed visual review.",
        )
    )
    story.append(
        matrix(
            ("Label", "Meaning", "What it does not mean"),
            (
                ("LIVE", "Connected to native runtime and visible or authoritative in its stated profile.", "Not automatically accepted across every viewport, seed, route, or profile."),
                ("GATED", "Implemented and testable behind an explicit mode.", "Not default shipping behavior and not proof that its visual gate passed."),
                ("PURE LAYER", "Compile-registered data implementation with bounded tests.", "Not connected to live rendering, physics, edits, or saves."),
                ("RESEARCH", "A source or candidate studied for an engine question.", "Not an implementation claim and not reproduced published results."),
            ),
            (22, 72, 80),
        )
    )
    story.extend([Spacer(1, 3 * mm), paragraph("CURRENT SOURCE BOUNDARIES", "h2")])
    story.append(
        matrix(
            ("System", "Current boundary", "Status"),
            (
                ("Near voxel authority", "Full 16^3 chunks own edits, collision, saves, simulation, and exact materials.", "LIVE"),
                ("Planetary far field", "One finest parent plus five annuli; six terrain entities; 15.36 km L_inf axis half-extent.", "ASTRAL LIVE / NATURAL GATED"),
                ("Far hydrography", "Shared-lattice render-only water and lava; no fluid authority.", "GATED"),
                ("Semantic cohorts", "Deterministic sparse L5 silhouettes under an exact selector ceiling.", "GATED / VISUAL PENDING"),
                ("Virtual voxel hierarchy", "Fixed-memory middle representation implemented as a pure data layer.", "PURE LAYER"),
                ("Evidence graph", "Typed compiler over explicit JSON candidates; native report adapter absent.", "TOOLING CONTRACT"),
            ),
            (39, 103, 32),
        )
    )
    story.extend(
        [
            Spacer(1, 3 * mm),
            callout(
                "Runtime gallery pending",
                "No live engine screenshot is embedded in this PDF. Four gallery slots remain intentionally empty until reviewed captures are bound to explicit route, seed, viewport, profile, and binary identity.",
                "coral",
            ),
            Spacer(1, 2 * mm),
            source_note(("README.md", "docs/CODEX_ENGINEERING_ATLAS.md")),
            PageBreak(),
        ]
    )

    # 03 - Representation architecture
    story.extend(
        section_header(
            "02 / REPRESENTATION ARCHITECTURE",
            "One world, several bounded views",
            "Representation becomes coarser with distance. Authority remains explicit: a render proxy cannot silently become edit, collision, or persistence truth.",
        )
    )
    story.extend(
        [
            svg_asset("docs/media/world-representation-architecture.svg", 174, 98),
            paragraph(
                "Project diagram: docs/media/world-representation-architecture.svg. Dashed and gated paths are status boundaries, not decorative uncertainty.",
                "caption",
            ),
        ]
    )
    story.append(
        matrix(
            ("Tier", "Owns", "Must never silently own"),
            (
                ("INTERACTION / NEAR", "Exact voxels, edits, collision, tools, saves, local simulation.", "No unbounded lifetime history."),
                ("MID / VIRTUAL BRICKS", "Conservative summaries and refinement error in a fixed cache.", "No live authority today; no eviction of sparse edits."),
                ("FAR / CLIPMAP", "Procedural height, material, silhouette, and gated descriptive layers.", "No user-edit replay, collider, save record, or global simulation."),
                ("CELESTIAL", "Analytic bodies and atmosphere beyond terrain reach.", "No dense planetary voxel allocation."),
                ("EVIDENCE", "Hashed reports, captures, claims, issues, and typed relationships.", "No invented measurements or acceptance from layout quality."),
            ),
            (39, 70, 65),
        )
    )
    story.extend(
        [
            Spacer(1, 2.5 * mm),
            callout(
                "Authority invariant",
                "A coarse parent remains ready before a finer child may disappear. Stale or identity-incompatible async work is rejected before install; safe retained work may only be reused under an explicit identity rule.",
                "teal",
            ),
            source_note(("README.md", "docs/PLANETARY_STREAMING_ARCHITECTURE.md")),
            PageBreak(),
        ]
    )

    # 04 - Planetary envelope
    story.extend(
        section_header(
            "03 / FLAGSHIP FORMULA SPREAD",
            "Geometric reach, fixed representation",
            "The horizon doubles per level while terrain topology, worker ownership, and public generated-payload ceilings remain fixed.",
        )
    )
    story.extend(
        [
            svg_asset("docs/media/planetary-budget-envelope.svg", 174, 90),
            paragraph(
                "Project diagram: docs/media/planetary-budget-envelope.svg. L_inf is the square topology axis half-extent, not a circular Euclidean radius.",
                "caption",
            ),
        ]
    )
    story.append(
        two_columns(
            [
                formula_card(
                    "Six-level recurrence",
                    "Delta_l = 16 * 2^l m\nR_l = 30 * Delta_l",
                    "l in {0,1,2,3,4,5}; R_5 = 15,360 m; exactly six terrain entities.",
                )
            ],
            [
                formula_card(
                    "Generated mesh envelope",
                    "B_mesh(V,I) = 48V + 4I\nV <= 35,000; I <= 150,000",
                    "Public CPU-generated payload ceiling: 2,280,000 bytes before renderer-owned copies.",
                )
            ],
        )
    )
    story.extend(
        [
            Spacer(1, 3 * mm),
            callout(
                "Exact consequence",
                "The no-cutout topology plus terminal L5 skirt produces 23,286 vertices, 110,760 indices, and 1,560,768 generated bytes. More travel changes sample identity, not resident terrain-entity count.",
                "blue",
            ),
            source_note(("docs/CODEX_ENGINEERING_ATLAS.md", "docs/FAR_TERMINAL_SKIRTS_V1.md")),
            PageBreak(),
        ]
    )

    # 05 - Morph and signed coordinates
    story.extend(
        section_header(
            "04 / INTEGER IDENTITY + LOCAL FLOATS",
            "Seam morphing without planetary float identity",
            "Global sample identity stays in checked signed integers. GPU positions remain local f32 values, and the outer three-cell band converges to the parent lattice.",
        )
    )
    story.append(
        two_columns(
            [
                formula_card(
                    "Morph coordinate",
                    "d = max(abs(x_local), abs(z_local))\nw = 3 * Delta_l\nt = clamp((d - (R_l - w)) / w, 0, 1)",
                    "Chebyshev distance matches the square-ring topology.",
                    compact=True,
                ),
                Spacer(1, 3 * mm),
                formula_card(
                    "Smoothstep",
                    "s(t) = t^2 * (3 - 2t)\ns(0)=0; s(1)=1\ns'(0)=s'(1)=0",
                    "Endpoint slope is zero; this reduces a sharp grade change but is not a visual verdict.",
                    compact=True,
                ),
            ],
            [
                formula_card(
                    "Displayed height",
                    "h_display = h_fine\n  + s(t) * (bilerp(h_parent) - h_fine)",
                    "Parent interpolation is evaluated on the next coarser global integer lattice.",
                    compact=True,
                ),
                Spacer(1, 3 * mm),
                formula_card(
                    "Euclidean partition",
                    "x = 16q + r\nq = x div_e 16\nr = x mod_e 16; 0 <= r < 16",
                    "Truncation toward zero is forbidden for negative world identity.",
                    compact=True,
                ),
            ],
        )
    )
    story.extend([Spacer(1, 4 * mm), paragraph("SIGNED COORDINATES ARE ALGEBRA", "h2")])
    story.append(
        matrix(
            ("World x", "Euclidean chunk q", "Local r", "Why it matters"),
            (
                ("15", "0", "15", "Last local voxel before the positive chunk boundary."),
                ("0", "0", "0", "Origin belongs to chunk zero."),
                ("-1", "-1", "15", "Negative neighbor, not chunk zero."),
                ("-16", "-1", "0", "Exact negative chunk boundary."),
            ),
            (24, 39, 24, 87),
        )
    )
    story.extend(
        [
            Spacer(1, 4 * mm),
            callout(
                "Fail closed",
                "Checked i64/i128 intermediates reject unrepresentable operations instead of wrapping a coordinate into another place. The same rule is shared by chunks, rings, material cells, virtual bricks, and semantic supertiles.",
                "amber",
            ),
            source_note(("docs/CODEX_ENGINEERING_ATLAS.md", "src/chunk.rs")),
            PageBreak(),
        ]
    )

    # 06 - Toroidal sampling and handoff
    story.extend(
        section_header(
            "05 / STREAMING OWNERSHIP",
            "Toroidal sampling and a fail-closed Near handoff",
            "Every level moves one fixed source window. Coverage removal is conservative: uncertainty keeps the parent instead of opening a sky hole.",
        )
    )
    story.extend(
        [
            svg_asset("docs/media/toroidal-cache-reuse.svg", 174, 96),
            paragraph(
                "Project diagram: docs/media/toroidal-cache-reuse.svg. Exact centre-sample reuse populations; mesh assembly and GPU upload remain separate work.",
                "caption",
            ),
        ]
    )
    story.append(
        two_columns(
            [
                formula_card(
                    "Near readiness bytes",
                    "33^2 bools = 1,089 B\nceil(60^2/64) * 8 = 456 B\ntotal = 1,545 B",
                    "The finest parent mask is a fixed 3,600-bit workset.",
                    compact=True,
                )
            ],
            [
                formula_card(
                    "Asymmetric stability",
                    "coverage gain: wait 0.5 s\ncoverage loss: restore immediately",
                    "Temporary overlap is preferred to a missing parent cell.",
                    compact=True,
                ),
            ],
        )
    )
    story.extend([Spacer(1, 4 * mm), paragraph("HANDOFF DECISION TABLE", "h2")])
    story.append(
        matrix(
            ("Observed state", "Parent decision", "Failure behavior"),
            (
                ("Current request proves stable Near coverage", "Remove covered parent cell after 0.5 s.", "Promotion is temporally stable."),
                ("Coverage unknown or missing", "Retain parent.", "No speculative hole."),
                ("Coverage stale, invalid, or unrepresentable", "Retain parent; reject result.", "Old or unsafe work cannot become geometry."),
                ("Coverage is lost", "Restore parent immediately.", "Safety recovers faster than cosmetic demotion."),
            ),
            (48, 59, 67),
        )
    )
    story.extend(
        [
            Spacer(1, 3 * mm),
            callout(
                "Rollback boundary",
                "A large incompatible anchor shift refills the same fixed window. It does not allocate a second unbounded history, and existing visible terrain remains until a complete validated replacement can install.",
                "teal",
            ),
            source_note(("docs/CODEX_ENGINEERING_ATLAS.md", "docs/PLANETARY_STREAMING_ARCHITECTURE.md", "docs/media/toroidal-cache-reuse.svg")),
            PageBreak(),
        ]
    )

    # 07 - Hydro and cohorts
    story.extend(
        section_header(
            "06 / GATED DESCRIPTIVE LAYERS",
            "Hydrography, river-bank grammar, and sparse silhouettes",
            "Both layers are deterministic and budgeted presentation. Neither can become fluid, voxel, edit, collision, navigation, physics, or save authority.",
        )
    )
    story.extend(
        [
            svg_asset("docs/media/river-bank-v3-cross-section.svg", 174, 90),
            paragraph(
                "Project diagram: docs/media/river-bank-v3-cross-section.svg. Natural V3 uses authored voxel-block visual units; it is not an erosion or shallow-water simulation.",
                "caption",
            ),
        ]
    )
    story.append(
        two_columns(
            [
                formula_card(
                    "Hydro + bank envelope",
                    "wet ring: V <= 3,721\nI <= 21,600\nB <= 265,008 bytes",
                    "GATED descriptive fluid plus Natural V3 bank grammar; Astral retains V1.",
                    compact=True,
                ),
            ],
            [
                formula_card(
                    "Cohort selector",
                    "one selected cell per 8 * 8 supertile\npublic candidates <= 9 * 9 = 81\npayload <= 104,976 bytes",
                    "Euclidean supertile identity is replayable across negative coordinates.",
                    compact=True,
                ),
            ],
        )
    )
    story.extend([Spacer(1, 4 * mm), paragraph("ATOMIC WORKER ENVELOPE", "h2")])
    story.append(
        matrix(
            ("Layer combination", "Maximum generated payload", "Install rule", "Rollback"),
            (
                ("Terrain + Hydro", "653,008 B", "Validate complete identity and both CPU payloads before asset creation.", HYDRO_ROLLBACK_LABEL),
                ("Optional L5 cohorts", "104,976 B", "One combined L5 cohort entity; reject impossible shape or budget excess.", COHORT_ROLLBACK_LABEL),
                ("Terrain + Hydro + cohorts", "757,984 B", "Any stale, malformed, or over-budget component publishes none of the new meshes.", "Disable gates independently"),
            ),
            (42, 35, 67, 30),
            font_size_padding=2.8,
        )
    )
    story.extend(
        [
            Spacer(1, 2 * mm),
            markup(
                "<b>PROOF BOUNDARY /</b> Unit tests and exact ceilings establish mechanics. Native Natural/Astral screenshots and telemetry must still be inspected together; average FPS cannot promote either layer.",
                "small",
            ),
            source_note(("docs/FAR_HYDROGRAPHIC_CONTINUITY_V1.md", "docs/NATURAL_RIVER_BANK_V3.md", "docs/FAR_SEMANTIC_COHORTS_V1.md", "docs/media/river-bank-v3-cross-section.svg")),
            PageBreak(),
        ]
    )

    # 08 - Virtual bricks
    story.extend(
        section_header(
            "07 / PURE MIDDLE REPRESENTATION",
            "Fixed-memory virtual bricks",
            "The hierarchy explores a conservative layer between exact chunks and the height-only far field. It is implemented, bounded, and deliberately not live-integrated.",
        )
    )
    story.append(
        two_columns(
            [
                formula_card(
                    "Summary payload",
                    "cell = u16 material + u8 occupancy + u8 error\ncell = 4 B\nbrick = 8^3 * 4 = 2,048 B",
                    "Fixed X-contiguous index: i(x,y,z) = x + 8z + 64y.",
                ),
                Spacer(1, 3 * mm),
                formula_card(
                    "Reduction",
                    "raw / summary = (2^L)^3 = 2^(3L)\nL2 = 64x\nL4 = 4,096x",
                    "Relative to the same four-byte raw voxel/material payload.",
                ),
            ],
            [
                formula_card(
                    "Native cache accounting",
                    "512 bricks\nB_cache = 1,093,632 B on verified 64-bit native\nactive tickets <= 128; ticket bytes = 7,168 B",
                    "wasm32 computes its own compile-time layout values.",
                    compact=True,
                ),
                Spacer(1, 3 * mm),
                callout(
                    "Current status",
                    "PURE LAYER. Compile-registered and tested, but not connected to renderer, physics, saves, or the live streaming scheduler.",
                    "amber",
                    width_mm=84,
                ),
            ],
        )
    )
    story.extend([Spacer(1, 4 * mm), paragraph("PRESSURE AND AUTHORITY", "h2")])
    story.append(
        matrix(
            ("Invariant", "Mechanism", "Failure or rollback boundary"),
            (
                ("Resident population never grows with travel", "Fixed 512-slot second-chance clock plus sorted fixed-capacity lookup.", "Reconstructible summaries are evicted deterministically."),
                ("Sparse edits remain authoritative", "Generator summaries and edit overlay are separate ownership domains.", "Cache pressure never evicts edit records."),
                ("Uncertainty does not become empty space", "Positive mass quantizes to at least one; error-only cells stay non-empty.", "Request refinement instead of manufacturing a hole."),
                ("Arithmetic cannot wrap the budget", "Checked byte-accounting returns None on multiplication overflow.", "Reject install; do not understate memory."),
                ("Rollback remains local", "The pure module is reconstructible and has no live pipeline ownership.", "Remove integration without rewriting saves or Near authority."),
            ),
            (44, 74, 56),
        )
    )
    story.extend(
        [
            Spacer(1, 3 * mm),
            source_note(("docs/VIRTUAL_VOXEL_HIERARCHY.md", "docs/CODEX_ENGINEERING_ATLAS.md")),
            PageBreak(),
        ]
    )

    # 09 - City math
    story.extend(
        section_header(
            "08 / PROJECT-AUTHORED HEURISTICS",
            "Road-first city planning math",
            "Candidate scoring is bounded, interpretable, and cheap enough to avoid a world scan. Its weights are engineering heuristics, not physical constants or new general theorems.",
        )
    )
    story.extend(
        [
            svg_asset("docs/media/city-site-score.svg", 174, 103),
            paragraph(
                "Project diagram: docs/media/city-site-score.svg. Six hard filters precede a weighted score and deterministic maximum selection.",
                "caption",
            ),
        ]
    )
    story.append(
        two_columns(
            [
                formula_card(
                    "Road grade fit",
                    ROAD_GRADE_FIT_FORMULA,
                    "A bounded terrain profile penalizes poor access inside the deterministic site score before voxel work.",
                    compact=True,
                )
            ],
            [
                formula_card(
                    "Smooth raised deck",
                    "s(t) = t^2 * (3 - 2t)\ndeck_y(t) = round(start_y\n  + (end_y - start_y) * s(t))",
                    "The same cubic family supplies a cheap deterministic grade envelope.",
                    compact=True,
                )
            ],
        )
    )
    story.extend([Spacer(1, 3 * mm), paragraph("PLANNING BOUNDARY", "h2")])
    story.append(
        matrix(
            ("Decision", "Chosen contract", "Rejected failure mode"),
            (
                ("Roads before buildings", "Reserve road corridors, bind frontage, then place structures.", "Buildings first and roads carved through them later."),
                ("Bounded candidate set", "Fixed candidate counts and local road/lot probes.", "Full-world voxel scans or per-frame city rebuilds."),
                (
                    "Clearance",
                    "Hard admission before project creation; execution pauses if clearance becomes unsafe.",
                    "Bots constructing around the active player or ship.",
                ),
                ("Pressure", "Edit queues yield when chunk streaming is behind.", "Hiding city cost inside horizon backlog."),
            ),
            (37, 69, 68),
        )
    )
    story.extend(
        [
            Spacer(1, 3 * mm),
            source_note(("docs/CITY_PLANNER_MATH.md", "src/bots.rs", "src/city.rs", "docs/media/city-site-score.svg")),
            PageBreak(),
        ]
    )

    # 10 - Evidence identity
    story.extend(
        section_header(
            "09 / DETERMINISTIC EVIDENCE IDENTITY",
            "Technical truth is separate from presentation",
            "A caller supplies a bounded local alias but cannot choose an authoritative node identity. Canonical source bytes and typed relationships remain visible downstream.",
        )
    )
    story.extend(
        [
            svg_asset("docs/media/evidence-lineage.svg", 174, 94),
            paragraph(
                "Project diagram: docs/media/evidence-lineage.svg. Source/build identity flows through native QA and canonical manifests; the report-to-graph adapter is still absent.",
                "caption",
            ),
        ]
    )
    identity_card = formula_card(
        "Authoritative node identity",
        "node_id = kind : sha256(canonical_json(identity))",
        "Canonical JSON uses sorted keys, finite values, deterministic scalar spelling, UTF-8, and no insignificant whitespace. Identity is not correctness or release readiness.",
        compact=True,
        width_mm=174,
    )
    story.append(Table([[identity_card]], colWidths=[174 * mm], hAlign="LEFT"))
    story.extend([Spacer(1, 3 * mm), paragraph("TWO STATUS DIMENSIONS", "h2")])
    story.append(
        matrix(
            ("Evidence classification", "Task state", "Non-equivalence"),
            (
                ("Passed / Observed / Rejected / Planned / Blocked", "planned / ready / running / blocked / review / complete / cancelled", "A complete task may produce a Rejected visual result."),
                ("Describes evidence", "Describes workflow", "Blocked evidence is not task blocked, and missing evidence is not zero."),
            ),
            (58, 66, 50),
        )
    )
    story.extend(
        [
            Spacer(1, 4 * mm),
            callout(
                "Integration limit",
                "The typed graph currently compiles explicit bounded JSON candidates. Native report.ron and QA manifests are not automatically translated; the manifest-to-candidate adapter remains unimplemented.",
                "coral",
            ),
            source_note(("docs/EVIDENCE_GRAPH_CONTRACT.md", "docs/EVIDENCE_MANIFEST_SCHEMA.md", "docs/media/evidence-lineage.svg")),
            PageBreak(),
        ]
    )

    # 11 - Budgets and rollback matrix
    story.extend(
        section_header(
            "10 / FIXED BUDGETS + FAILURE CONTAINMENT",
            "Every ambitious system publishes its stop condition",
            "The useful promise is not infinite dense voxels at zero cost. It is bounded work with graceful representation changes and a reversible boundary.",
        )
    )
    story.append(
        matrix(
            ("System", "Hard envelope", "Failure behavior", "Rollback boundary"),
            (
                ("Far terrain", "6 levels; V <= 35,000; I <= 150,000; B <= 2,280,000", "Reject stale, malformed, over-budget, or identity-invalid build before install.", "Disable gated profile route; retain Near and current parent."),
                ("Near handoff", "1,545 B readiness/mask workset", "Unknown or stale coverage retains parent; loss restores immediately.", "Refill same fixed window; no history growth."),
                ("Hydro", "6 fluid entities; 1,590,048 generated B total", "Atomic terrain/fluid rejection; dry ring emits no fluid entity.", "Hydro gate off; no persisted data migration."),
                ("Semantic cohorts", "81 candidates; 1 entity; 104,976 B", "Fail-closed parsing; impossible shape or overflow rejects payload.", "Cohort gate off; terrain and saves unchanged."),
                ("Virtual bricks", "512 residents; 128 tickets", "Deterministic eviction; checked accounting; authority remains separate.", "Remove pure integration; reconstruct summaries."),
                ("Evidence graph", "64 inputs; 12k nodes; 32k edges; 16 MiB", "All-or-nothing compile; no permissive legacy mode.", "Remove tools/evidence without changing inputs/builders."),
                ("Agent control", "64 KiB control file; bounded strings/vectors; strict sequence", "Reject stale/reused identity, non-finite pose, unsafe path, or oversized input.", "Explicit isolated session; runtime exit is separate."),
                ("City planner", "Fixed candidate counts; local probes; bounded edit queues", "Hard-invalid sites do not consume voxel edit budget.", "Yield planning when streaming pressure rises."),
            ),
            (31, 48, 57, 38),
            font_size_padding=3.2,
        )
    )
    story.extend([Spacer(1, 4 * mm)])
    story.append(
        two_columns(
            [
                callout(
                    "Pressure order",
                    "Reduce detail, cadence, or promotion radius before shortening the far silhouette, evicting authority, or allowing queues to grow with travel distance.",
                    "teal",
                    width_mm=84,
                )
            ],
            [
                callout(
                    "Unknown means unknown",
                    "Corrupt, stale, unsupported, or unobserved state remains a visible diagnostic. It is never converted to zero or silently labeled healthy.",
                    "amber",
                    width_mm=84,
                )
            ],
        )
    )
    story.extend(
        [
            Spacer(1, 4 * mm),
            callout(
                "Novel solution rule",
                "Record baseline, alternatives, fixed budget, measured distribution, invalidating assumptions, failure mode, and rollback boundary. Simpler code that wins the real metric is the more advanced solution.",
                "blue",
            ),
            source_note(("docs/ELITE_WORLD_SYSTEMS_STANDARD.md", "docs/PLANETARY_STREAMING_ARCHITECTURE.md", "src/agent_control.rs")),
            PageBreak(),
        ]
    )

    # 12 - Codex engineering loop
    story.extend(
        section_header(
            "11 / CODEX ENGINEERING LOOP",
            "Research becomes reversible engine work",
            "Each step preserves the distinction between an idea, a bounded implementation, an observation, and an accepted claim.",
        )
    )
    loop_rows = (
        ("01", "RESEARCH QUESTION", "Name the world-system question and the exact user-visible failure."),
        ("02", "AUTHORITY BOUNDARY", "Declare who owns edit, collision, persistence, rendering, and simulation state."),
        ("03", "FIXED BUDGET", "Cap bytes, entities, work, queues, tasks, and per-frame install population."),
        ("04", "IMPLEMENT", "Carry world identity, epoch, sequence, and stale-result rejection through async work."),
        ("05", "ADVERSARIAL TEST", "Exercise negative/extreme coordinates, order independence, pressure, overflow, and malformed input."),
        ("06", "ONE-BINARY ROUTES", "Compare like-for-like arms from the same release executable and explicit route identity."),
        ("07", "INSPECT TOGETHER", "Read full-size screenshots and telemetry together; neither can substitute for the other."),
        ("08", "RETAIN / REVISE / ROLL BACK", "Promote only the narrow claim supported by evidence; preserve rejected observations."),
    )
    loop_data: list[list[Any]] = []
    for index, (number, label, detail) in enumerate(loop_rows):
        loop_data.append(
            [
                paragraph(number, "step"),
                paragraph(label, "step"),
                paragraph(detail, "body"),
            ]
        )
    loop_table = Table(loop_data, colWidths=[14 * mm, 47 * mm, 113 * mm], hAlign="LEFT")
    loop_commands: list[tuple[Any, ...]] = [
        ("VALIGN", (0, 0), (-1, -1), "MIDDLE"),
        ("GRID", (0, 0), (-1, -1), 0.35, line),
        ("LEFTPADDING", (0, 0), (-1, -1), 7),
        ("RIGHTPADDING", (0, 0), (-1, -1), 7),
        ("TOPPADDING", (0, 0), (-1, -1), 7),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 6),
        ("BACKGROUND", (0, 0), (0, -1), pale),
    ]
    for row_index in range(len(loop_rows)):
        loop_commands.append(
            ("BACKGROUND", (1, row_index), (-1, row_index), white if row_index % 2 == 0 else soft)
        )
    loop_table.setStyle(TableStyle(loop_commands))
    story.append(loop_table)
    story.extend([Spacer(1, 5 * mm)])
    story.append(
        two_columns(
            [
                callout(
                    "Why screenshots",
                    "A beautiful frame can conceal unbounded work, stale authority, or a broken alternate route.",
                    "coral",
                    width_mm=84,
                )
            ],
            [
                callout(
                    "Why telemetry",
                    "Clean counters can conceal a hole, pop, unreadable UI, unstable silhouette, or wrong material.",
                    "blue",
                    width_mm=84,
                )
            ],
        )
    )
    story.extend(
        [
            Spacer(1, 5 * mm),
            paragraph("CODEX OPERATOR CONTRACT", "h2"),
            bullet("Batch source changes, run deterministic checks, and use one deliberate native rebuild when compiled Rust changes."),
            bullet("Keep user saves and unrelated dirty files outside the experiment boundary."),
            bullet("Record exact toolchain, target, profile, seed, route, viewport, binary identity, and known limits."),
            bullet("Do not label a fallback transport as direct or a completed PNG as a visual pass."),
            source_note(("README.md", "docs/ELITE_WORLD_SYSTEMS_STANDARD.md", "docs/RESPONSIVE_VISUAL_QA.md")),
            PageBreak(),
        ]
    )

    # 13 - Research routes
    story.extend(
        section_header(
            "12 / RESEARCH + PROOF BOUNDARY",
            "Ideas enter as sources; claims leave through gates",
            "Research routes are traceable to original publishers. Studying a technique does not mean Voxel Native ships it or reproduces its published result.",
        )
    )
    story.extend(
        [
            svg_asset("docs/media/research-routes.svg", 174, 62),
            paragraph(
                "Project diagram: docs/media/research-routes.svg. READ -> TRANSLATE -> BOUND -> TEST -> ACCEPT / REVISE / REJECT.",
                "caption",
            ),
        ]
    )
    story.append(
        matrix(
            ("Research input", "Engine question", "Current claim"),
            (
                ("Virtual Horizon Method", "Can a height-map abstraction bound far-world visibility under an accuracy/latency trade-off?", "RESEARCH - no VHM runtime claim."),
                ("Multiscale pine rendering", "Which representation survives as dense natural detail recedes?", "RESEARCH - no multiscale tree-shader runtime claim."),
                ("Generative Adversarial Shaders", "Which decomposed shader stages help without unstable temporal artifacts?", "RESEARCH - no learned shader ships."),
            ),
            (46, 86, 42),
        )
    )
    story.extend([Spacer(1, 4 * mm), paragraph("TRANSFER CHECK", "h2")])
    story.append(
        matrix(
            ("Stage", "Required record", "Disallowed shortcut"),
            (
                ("READ", "Original publisher URL, bounded local notes, exact question.", "Treating a secondary summary as implementation proof."),
                ("TRANSLATE", "Authority boundary and engine-specific failure metric.", "Copying a paper label onto unrelated code."),
                ("BOUND", "Work, memory, population, and rollback envelope.", "Average cost without a hard ceiling."),
                ("TEST", "Deterministic/adversarial checks plus same-binary native routes where visual.", "Unit tests alone for perceptual acceptance."),
                ("PROMOTE", "Narrow claim, evidence identity, known limits, and reviewed images.", "A link, screenshot, or benchmark promoted by aesthetics."),
            ),
            (29, 91, 54),
        )
    )
    story.extend(
        [
            Spacer(1, 4 * mm),
            callout(
                "Research is visible",
                "The Voxel Discovery Atlas tracks adopted, prototype, deferred, and rejected routes across clipmaps, sparse volumes, virtual texturing, splatting, watersheds, natural structures, QA, and implicit geometry.",
                "teal",
            ),
            source_note(("docs/VOXEL_DISCOVERY_ATLAS.md", "docs/FAR_WORLD_RENDERING_RESEARCH.md")),
            PageBreak(),
        ]
    )

    # 14 - Runtime gallery and known limits
    story.extend(
        section_header(
            "13 / DECLARED LIMITS",
            "Runtime gallery pending",
            "The empty gallery is deliberate. This source-authored atlas does not scan QA runs or select a convenient image; visual evidence requires explicit manifest identity and full-size review.",
        )
    )
    gallery_cells: list[list[Any]] = []
    gallery_specs = (
        ("NATURAL OVERVIEW", "PENDING - matched same-binary visual gate not attached"),
        ("ASTRAL OVERVIEW", "PENDING - reviewed capture not attached"),
        ("NEAR / FAR HANDOFF", "PENDING - route + telemetry + visual review required"),
        ("BUILD STUDIO", "PENDING - interaction and responsive matrix evidence required"),
    )
    for start in range(0, len(gallery_specs), 2):
        row: list[Any] = []
        for label, detail in gallery_specs[start : start + 2]:
            cell = Table(
                [[paragraph(label, "callout_label")], [paragraph(detail, "small")]],
                colWidths=[82 * mm],
            )
            cell.setStyle(
                TableStyle(
                    [
                        ("BACKGROUND", (0, 0), (-1, -1), pale_coral),
                        ("BOX", (0, 0), (-1, -1), 0.7, coral),
                        ("LEFTPADDING", (0, 0), (-1, -1), 9),
                        ("RIGHTPADDING", (0, 0), (-1, -1), 9),
                        ("TOPPADDING", (0, 0), (-1, -1), 10),
                        ("BOTTOMPADDING", (0, 0), (-1, -1), 10),
                    ]
                )
            )
            row.append(cell)
        gallery_cells.append(row)
    gallery = Table(gallery_cells, colWidths=[87 * mm, 87 * mm], hAlign="LEFT")
    gallery.setStyle(
        TableStyle(
            [
                ("VALIGN", (0, 0), (-1, -1), "TOP"),
                ("LEFTPADDING", (0, 0), (-1, -1), 3),
                ("RIGHTPADDING", (0, 0), (-1, -1), 3),
                ("TOPPADDING", (0, 0), (-1, -1), 3),
                ("BOTTOMPADDING", (0, 0), (-1, -1), 3),
            ]
        )
    )
    story.append(gallery)
    story.extend([Spacer(1, 4 * mm), paragraph("KNOWN LIMITS THAT REMAIN VISIBLE", "h2")])
    story.append(
        matrix(
            ("Boundary", "Declared limit", "Next proof"),
            (
                ("Natural far terrain", "Explicitly gated; matched visual acceptance remains pending.", "Same release binary, fixed routes, required viewport/DPI cells, full-size review."),
                ("Virtual hierarchy", "Pure data layer; no live renderer, physics, save, or scheduler connection.", "Integration contract plus cross-tier edit and identity proof."),
                ("Far procedural layers", "Far does not replay sparse user edits today.", "Bounded edit projection with conservative invalidation."),
                ("Hydro", "No flow, depth, refraction, buoyancy, waves, foam, or collider.", "Separate feature contracts; no inference from colour quads."),
                ("Semantic cohorts", "Placeholder silhouettes; composition and pop acceptance remain open.", "Natural/Astral on/off routes with telemetry agreement."),
                ("Evidence graph", "No native report/manifest adapter; no production-size benchmark.", "Explicit adapter and throughput evidence without semantic drift."),
                ("Responsive QA", "Several viewport and 100/150/200 percent scale cells remain outstanding.", "Capture and inspect every required matrix cell."),
                ("License", "No reuse license has been declared.", "Maintainer-selected license before redistribution rights are implied."),
            ),
            (39, 77, 58),
            font_size_padding=3.3,
        )
    )
    story.extend(
        [
            Spacer(1, 4 * mm),
            callout(
                "Promotion rule",
                "Name mode and authority; bind source and binary identity; record seed/profile/route/viewport; validate populations and stale-result identity; inspect every image; report distributions; retain rejected evidence; roll back when required.",
                "amber",
            ),
            source_note(("docs/CODEX_ENGINEERING_ATLAS.md", "docs/RESPONSIVE_VISUAL_QA.md")),
            PageBreak(),
        ]
    )

    # 15 - References and final checklist
    story.extend(
        section_header(
            "14 / REFERENCES + VERIFICATION",
            "Official routes and final artifact checklist",
            "Links are research and platform references. They are not evidence that an external technique or result ships in Voxel Native.",
        )
    )
    reference_rows: list[list[Any]] = []
    for index in range(0, len(OFFICIAL_REFERENCES), 2):
        left_ref = OFFICIAL_REFERENCES[index]
        left_cell = reference_paragraph(*left_ref)
        if index + 1 < len(OFFICIAL_REFERENCES):
            right_cell = reference_paragraph(*OFFICIAL_REFERENCES[index + 1])
        else:
            right_cell = paragraph("", "reference")
        reference_rows.append([left_cell, right_cell])
    refs_table = Table(reference_rows, colWidths=[87 * mm, 87 * mm], hAlign="LEFT")
    refs_commands: list[tuple[Any, ...]] = [
        ("VALIGN", (0, 0), (-1, -1), "TOP"),
        ("GRID", (0, 0), (-1, -1), 0.35, line),
        ("LEFTPADDING", (0, 0), (-1, -1), 6),
        ("RIGHTPADDING", (0, 0), (-1, -1), 6),
        ("TOPPADDING", (0, 0), (-1, -1), 5),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 5),
    ]
    for row_index in range(len(reference_rows)):
        refs_commands.append(
            ("BACKGROUND", (0, row_index), (-1, row_index), white if row_index % 2 == 0 else soft)
        )
    refs_table.setStyle(TableStyle(refs_commands))
    story.append(refs_table)
    story.extend([Spacer(1, 4 * mm), paragraph("FINAL VERIFICATION CHECKLIST", "h2")])
    checklist_rows = (
        ("BUILDER", "[x]", f"All {EXPECTED_INPUT_COUNT} immutable source snapshots bounded, anchored, and fingerprinted."),
        ("BUILDER", "[x]", "All eight passive ASCII SVGs safely parsed and embedded with repo-relative captions."),
        ("BUILDER", "[x]", f"Exactly {EXPECTED_PAGE_COUNT} strict A4 pages; envelope, compression, metadata, IDs, and links validated."),
        ("BUILDER", "[x]", "PDF text rejects workstation paths, controls, replacement glyphs, and Unicode dashes."),
        ("BUILDER", "[x]", "NO RUNTIME RELEASE VERDICT and RUNTIME GALLERY PENDING remain extractable text."),
        ("OPERATOR", "[ ]", "Render every page to PNG with Poppler using the final output bytes."),
        ("OPERATOR", "[ ]", "Inspect every page at full size for clipping, overlap, black squares, broken SVG text, and weak contrast."),
        ("OPERATOR", "[ ]", "Verify page numbering, headers/footers, link readability, formula legibility, and empty gallery honesty."),
        ("OPERATOR", "[ ]", "Record final PDF SHA-256 only after the rendered-page review is complete."),
    )
    story.append(
        matrix(
            ("Owner", "State", "Check"),
            checklist_rows,
            (27, 18, 129),
            font_size_padding=3.2,
        )
    )
    story.extend(
        [
            Spacer(1, 4 * mm),
            callout(
                "Final boundary",
                f"Aggregate source fingerprint: {fingerprint}. Builder SHA-256: {builder_sha}. The PDF is complete as a project-authored technical atlas only after separate rendered-page inspection; the runtime gallery and runtime release verdict remain pending.",
                "coral",
            ),
            paragraph(
                f"Source boundary: {EXPECTED_INPUT_COUNT} immutable inputs enumerated by the builder "
                "contract; authoritative Rust anchors include src/chunk.rs | src/bots.rs | "
                "src/city.rs | src/agent_control.rs.",
                "small",
            ),
        ]
    )

    try:
        document.build(story, canvasmaker=InvariantCanvas)
        data = output_buffer.getvalue()
    except AtlasBuildError:
        raise
    except Exception as error:
        raise AtlasBuildError(f"ReportLab could not build the atlas: {error}") from error

    validate_built_pdf(data, PdfReader, TextStringObject, identity)
    return BuiltPdf(
        data=data,
        sha256=hashlib.sha256(data).hexdigest(),
        document_id_hex=document_id_hex,
    )


def dereference_pdf_object(value: Any) -> Any:
    return value.get_object() if hasattr(value, "get_object") else value


def pdf_id_hex(value: Any, text_string_type: type[str]) -> str:
    resolved = dereference_pdf_object(value)
    try:
        if isinstance(resolved, bytes):
            raw = bytes(resolved)
        elif type(resolved) is text_string_type:
            original_bytes = resolved.original_bytes
            if not isinstance(original_bytes, bytes):
                raise TypeError("original_bytes is not a byte string")
            raw = bytes(original_bytes)
        else:
            raise TypeError(f"unsupported PDF ID value {type(resolved).__name__}")
    except Exception as error:
        raise AtlasBuildError(
            "generated PDF document ID cannot be recovered as authoritative bytes"
        ) from error
    if not raw:
        raise AtlasBuildError("generated PDF document ID is empty")
    return raw.hex().upper()


def pdf_filter_names(stream: Any) -> set[str]:
    resolved = dereference_pdf_object(stream)
    filters = dereference_pdf_object(resolved.get("/Filter"))
    if filters is None:
        return set()
    if isinstance(filters, (list, tuple)):
        return {str(dereference_pdf_object(value)) for value in filters}
    return {str(filters)}


def validate_no_active_pdf_objects(initial: Any) -> None:
    """Walk the bounded generated object graph and reject active PDF features."""

    pending = [initial]
    seen_indirect: set[tuple[int, int]] = set()
    seen_direct: set[int] = set()
    object_count = 0
    while pending:
        raw = pending.pop()
        if hasattr(raw, "idnum") and hasattr(raw, "generation"):
            indirect_identity = (int(raw.idnum), int(raw.generation))
            if indirect_identity in seen_indirect:
                continue
            seen_indirect.add(indirect_identity)
        resolved = dereference_pdf_object(raw)
        if isinstance(resolved, (dict, list, tuple)):
            direct_identity = id(resolved)
            if direct_identity in seen_direct:
                continue
            seen_direct.add(direct_identity)
            object_count += 1
            if object_count > MAX_PDF_OBJECTS:
                raise AtlasBuildError(
                    f"generated PDF exceeds the {MAX_PDF_OBJECTS}-object validation cap"
                )

        if isinstance(resolved, dict):
            object_type = str(dereference_pdf_object(resolved.get("/Type")))
            if object_type == "/Font" or "/BaseFont" in resolved:
                base_font = str(dereference_pdf_object(resolved.get("/BaseFont")))
                subtype = str(dereference_pdf_object(resolved.get("/Subtype")))
                encoding = str(dereference_pdf_object(resolved.get("/Encoding")))
                forbidden_font_keys = {
                    "/FontFile",
                    "/FontFile2",
                    "/FontFile3",
                    "/ToUnicode",
                }.intersection(resolved)
                if (
                    base_font not in ALLOWED_PDF_BASE_FONT_NAMES
                    or subtype != "/Type1"
                    or encoding != "/WinAnsiEncoding"
                    or forbidden_font_keys
                ):
                    raise AtlasBuildError(
                        "generated PDF contains a non-canonical or externally resolved font: "
                        f"base={base_font}, subtype={subtype}, encoding={encoding}, "
                        f"forbidden_keys={sorted(forbidden_font_keys)}"
                    )
            action_name = str(dereference_pdf_object(resolved.get("/S")))
            if action_name == "/URI":
                uri_value = dereference_pdf_object(resolved.get("/URI"))
                if (
                    "/URI" not in resolved
                    or not isinstance(uri_value, str)
                    or not uri_value.startswith("https://")
                    or uri_value not in OFFICIAL_URIS
                    or "/Next" in resolved
                ):
                    raise AtlasBuildError(
                        "generated PDF contains a URI action outside the exact HTTPS allowlist"
                    )
                guard_public_text(uri_value)
            for key, child in resolved.items():
                name = str(key)
                if name in FORBIDDEN_PDF_KEYS:
                    raise AtlasBuildError(f"generated PDF contains forbidden active key {name}")
                scalar = dereference_pdf_object(child)
                if name == "/S" and str(scalar) in FORBIDDEN_PDF_ACTIONS:
                    raise AtlasBuildError(
                        f"generated PDF contains forbidden action {scalar}"
                    )
                if name in {"/Type", "/Subtype"} and str(scalar) in FORBIDDEN_PDF_TYPES:
                    raise AtlasBuildError(
                        f"generated PDF contains forbidden active object type {scalar}"
                    )
                pending.append(child)
        elif isinstance(resolved, (list, tuple)):
            pending.extend(resolved)


def validate_built_pdf(
    data: bytes,
    pdf_reader_type: Any,
    text_string_type: type[str],
    identity: AtlasDocumentIdentity,
) -> int:
    if len(data) <= 1024:
        raise AtlasBuildError("generated PDF is unexpectedly small")
    if len(data) > MAX_PDF_BYTES:
        raise AtlasBuildError(f"generated PDF exceeds the {MAX_PDF_BYTES}-byte output cap")
    stripped = data.rstrip(b"\x00\t\n\r\f ")
    if (
        not data.startswith(b"%PDF-")
        or not stripped.endswith(b"%%EOF")
        or stripped.count(b"%%EOF") != 1
        or re.search(rb"startxref\s+\d+\s+%%EOF$", stripped) is None
    ):
        raise AtlasBuildError("generated output does not have a complete PDF envelope")

    try:
        reader = pdf_reader_type(io.BytesIO(data), strict=True)
    except Exception as error:
        raise AtlasBuildError(f"generated PDF cannot be reopened structurally: {error}") from error
    if reader.is_encrypted:
        raise AtlasBuildError("generated PDF must not be encrypted")
    validate_no_active_pdf_objects(reader.trailer)
    if len(reader.pages) != EXPECTED_PAGE_COUNT:
        raise AtlasBuildError(
            f"atlas layout produced {len(reader.pages)} pages; expected exactly {EXPECTED_PAGE_COUNT}"
        )

    root = dereference_pdf_object(reader.trailer.get("/Root"))
    if root is None:
        raise AtlasBuildError("generated PDF has no catalog")
    for key in ("/OpenAction", "/AA", "/AcroForm"):
        if root.get(key) is not None:
            raise AtlasBuildError(f"generated PDF contains forbidden catalog entry {key}")
    names = dereference_pdf_object(root.get("/Names"))
    if names is not None:
        for key in ("/JavaScript", "/EmbeddedFiles"):
            if names.get(key) is not None:
                raise AtlasBuildError(f"generated PDF contains forbidden names entry {key}")

    expected_width = 595.2755905511812
    expected_height = 841.8897637795277
    page_texts: list[str] = []
    observed_uris: set[str] = set()
    for page_index, page in enumerate(reader.pages):
        if page.get("/AA") is not None:
            raise AtlasBuildError(f"generated PDF page {page_index + 1} contains an additional action")
        media = tuple(float(value) for value in page.mediabox)
        crop = tuple(float(value) for value in page.cropbox)
        expected_box = (0.0, 0.0, expected_width, expected_height)
        if any(abs(actual - expected) > 0.02 for actual, expected in zip(media, expected_box)):
            raise AtlasBuildError(f"generated PDF page {page_index + 1} is not exact A4")
        if any(abs(actual - expected) > 0.02 for actual, expected in zip(crop, expected_box)):
            raise AtlasBuildError(f"generated PDF page {page_index + 1} has an unexpected crop box")
        if int(page.get("/Rotate", 0) or 0) % 360 != 0:
            raise AtlasBuildError(f"generated PDF page {page_index + 1} has unexpected rotation")

        raw_contents = page.get("/Contents")
        if raw_contents is None:
            raise AtlasBuildError(f"generated PDF page {page_index + 1} has no content stream")
        content_streams = (
            list(dereference_pdf_object(raw_contents))
            if isinstance(dereference_pdf_object(raw_contents), (list, tuple))
            else [raw_contents]
        )
        if not content_streams or any(
            "/FlateDecode" not in pdf_filter_names(stream) for stream in content_streams
        ):
            raise AtlasBuildError(
                f"generated PDF page {page_index + 1} is missing Flate-compressed content"
            )

        text = page.extract_text() or ""
        if not text.isascii():
            unexpected = sorted({ord(character) for character in text if ord(character) >= 0x80})
            labels = ", ".join(f"U+{codepoint:04X}" for codepoint in unexpected)
            raise AtlasBuildError(
                f"generated PDF page {page_index + 1} contains unexpected non-ASCII text: {labels}"
            )
        guard_public_text(text)
        normalized_text = " ".join(text.split())
        expected_heading = EXPECTED_PAGE_HEADINGS[page_index]
        if expected_heading not in normalized_text:
            raise AtlasBuildError(
                f"generated PDF page {page_index + 1} is missing heading {expected_heading!r}"
            )
        page_texts.append(text)

        annotations = dereference_pdf_object(page.get("/Annots")) or []
        for annotation_reference in annotations:
            annotation = dereference_pdf_object(annotation_reference)
            if str(annotation.get("/Subtype")) != "/Link" or annotation.get("/Dest") is not None:
                raise AtlasBuildError(
                    f"generated PDF page {page_index + 1} contains a non-URI annotation"
                )
            action = dereference_pdf_object(annotation.get("/A"))
            if action is None or str(action.get("/S")) != "/URI" or action.get("/Next") is not None:
                raise AtlasBuildError(
                    f"generated PDF page {page_index + 1} contains an unsafe link action"
                )
            uri = str(dereference_pdf_object(action.get("/URI")))
            guard_public_text(uri)
            if not uri.startswith("https://") or uri not in OFFICIAL_URIS:
                raise AtlasBuildError(f"generated PDF contains a URI outside the allowlist: {uri}")
            observed_uris.add(uri)

    if observed_uris != OFFICIAL_URIS:
        missing = sorted(OFFICIAL_URIS - observed_uris)
        extra = sorted(observed_uris - OFFICIAL_URIS)
        raise AtlasBuildError(
            f"generated PDF URI annotations do not match the allowlist; missing={missing}, extra={extra}"
        )

    extracted = "\n".join(page_texts)
    required_phrases = (
        "CODEX ENGINEERING ATLAS",
        "NO RUNTIME RELEASE VERDICT",
        "RUNTIME GALLERY PENDING",
    )
    absent = [phrase for phrase in required_phrases if phrase not in extracted]
    compact_extracted = re.sub(r"\s+", "", extracted)
    for label, expected_identity in (
        ("aggregate source fingerprint", identity.fingerprint),
        ("builder SHA-256", identity.builder_sha),
    ):
        if expected_identity not in compact_extracted:
            absent.append(f"full {label}")
    if "node_id=kind:sha256(canonical_json(identity))" not in compact_extracted:
        absent.append("node_id = kind : sha256(canonical_json(identity))")
    if absent:
        raise AtlasBuildError(f"generated PDF text is missing required contract phrases: {absent}")
    guard_public_text(extracted)

    metadata = reader.metadata or {}
    expected_metadata = {
        "/Title": PDF_METADATA["title"],
        "/Author": PDF_METADATA["author"],
        "/Creator": PDF_METADATA["creator"],
        "/Producer": PDF_METADATA["producer"],
        "/Subject": PDF_METADATA["subject"],
        "/Keywords": PDF_METADATA["keywords"],
        "/CreationDate": CANONICAL_PDF_DATE,
        "/ModDate": CANONICAL_PDF_DATE,
        "/Trapped": "/False",
    }
    if set(metadata.keys()) != set(expected_metadata):
        raise AtlasBuildError(
            "generated PDF metadata fields are not canonical: "
            f"observed={sorted(metadata.keys())}, expected={sorted(expected_metadata)}"
        )
    mismatched_metadata = {
        key: (metadata.get(key), expected)
        for key, expected in expected_metadata.items()
        if metadata.get(key) != expected
    }
    if mismatched_metadata:
        raise AtlasBuildError(f"generated PDF metadata is not canonical: {mismatched_metadata}")
    metadata_text = " ".join(str(value) for value in metadata.values())
    guard_public_text(metadata_text)

    trailer_ids = dereference_pdf_object(reader.trailer.get("/ID"))
    if not isinstance(trailer_ids, (list, tuple)) or len(trailer_ids) != 2:
        raise AtlasBuildError("generated PDF has no canonical two-part document ID")
    if [pdf_id_hex(value, text_string_type) for value in trailer_ids] != [
        identity.document_id_hex,
        identity.document_id_hex,
    ]:
        raise AtlasBuildError("generated PDF document ID does not match its build identity")
    return len(reader.pages)


def prepare_output_parent(root: Path, target: OutputTarget) -> tuple[int, int]:
    parent = target.path.parent
    assert_no_reparse_components(
        parent,
        root,
        allow_missing=True,
        label="PDF output parent",
    )
    current = lexical_absolute(root)
    for part in parent.relative_to(current).parts:
        current /= part
        info = lstat_or_none(current)
        if info is None:
            try:
                current.mkdir()
            except FileExistsError:
                pass
            except OSError as error:
                raise AtlasBuildError(
                    f"could not create safe PDF output directory {current}: {error}"
                ) from error
            info = lstat_or_none(current)
        if info is None or is_reparse_stat(info) or not stat.S_ISDIR(info.st_mode):
            raise AtlasBuildError(f"PDF output parent is not a safe directory: {current}")

    assert_no_reparse_components(
        parent,
        root,
        allow_missing=False,
        label="PDF output parent",
    )
    if not is_relative_to(target.path.resolve(strict=False), target.allowed_root.resolve(strict=True)):
        raise AtlasBuildError("PDF output parent changed its resolved containment")
    parent_info = os.stat(parent, follow_symlinks=False)
    return (parent_info.st_dev, parent_info.st_ino)


def revalidate_output_target(
    root: Path,
    target: OutputTarget,
    parent_identity: tuple[int, int],
    force: bool,
) -> None:
    assert_no_reparse_components(
        target.path,
        root,
        allow_missing=True,
        label="PDF output",
    )
    parent_info = os.stat(target.path.parent, follow_symlinks=False)
    if is_reparse_stat(parent_info) or not stat.S_ISDIR(parent_info.st_mode):
        raise AtlasBuildError("PDF output parent became unsafe before publication")
    if (parent_info.st_dev, parent_info.st_ino) != parent_identity:
        raise AtlasBuildError("PDF output parent identity changed before publication")
    if not is_relative_to(target.path.resolve(strict=False), target.allowed_root.resolve(strict=True)):
        raise AtlasBuildError("PDF output changed its resolved containment before publication")

    output_info = lstat_or_none(target.path)
    if output_info is not None and (
        is_reparse_stat(output_info) or not stat.S_ISREG(output_info.st_mode)
    ):
        raise AtlasBuildError("PDF output became a symlink, reparse point, or non-regular file")
    if output_info is not None and not force:
        raise AtlasBuildError(f"output appeared during no-clobber publication: {target.path}")


def write_validated_temp(target: OutputTarget, built: BuiltPdf) -> Path:
    try:
        descriptor, temp_name = tempfile.mkstemp(
            prefix=f".{target.path.name}.",
            suffix=".tmp",
            dir=target.path.parent,
        )
    except OSError as error:
        raise AtlasBuildError(f"could not create the same-directory PDF temporary file: {error}") from error

    temp_path = Path(temp_name)
    descriptor_open = True
    try:
        handle = os.fdopen(descriptor, "wb", closefd=True)
        descriptor_open = False
        with handle:
            view = memoryview(built.data)
            written = 0
            while written < len(view):
                count = handle.write(view[written:])
                if count is None or count <= 0:
                    raise AtlasBuildError("could not write the complete PDF temporary file")
                written += count
            handle.flush()
            os.fsync(handle.fileno())
    except Exception:
        if descriptor_open:
            try:
                os.close(descriptor)
            except OSError:
                pass
        try:
            temp_path.unlink(missing_ok=True)
        except OSError:
            pass
        raise

    info = lstat_or_none(temp_path)
    if (
        info is None
        or is_reparse_stat(info)
        or not stat.S_ISREG(info.st_mode)
        or info.st_size != len(built.data)
    ):
        temp_path.unlink(missing_ok=True)
        raise AtlasBuildError("PDF temporary file identity or size is invalid")
    return temp_path


def verify_temp_bytes(temp_path: Path, built: BuiltPdf) -> tuple[int, int]:
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor: int | None = None
    try:
        descriptor = os.open(temp_path, flags)
        handle = os.fdopen(descriptor, "rb", closefd=True)
        descriptor = None
        with handle:
            data = handle.read(MAX_PDF_BYTES + 1)
            after = os.fstat(handle.fileno())
    except OSError as error:
        raise AtlasBuildError(f"could not revalidate the PDF temporary file: {error}") from error
    finally:
        if descriptor is not None:
            try:
                os.close(descriptor)
            except OSError:
                pass
    if (
        len(data) > MAX_PDF_BYTES
        or after.st_size != len(data)
        or hashlib.sha256(data).hexdigest() != built.sha256
        or data != built.data
    ):
        raise AtlasBuildError("PDF temporary bytes changed before publication")
    final_info = lstat_or_none(temp_path)
    if (
        final_info is None
        or is_reparse_stat(final_info)
        or not stat.S_ISREG(final_info.st_mode)
        or not os.path.samestat(after, final_info)
    ):
        raise AtlasBuildError("PDF temporary file identity changed before publication")
    return (after.st_dev, after.st_ino)


def fsync_directory_best_effort(directory: Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    try:
        descriptor = os.open(directory, flags)
    except OSError:
        return
    try:
        os.fsync(descriptor)
    except OSError:
        pass
    finally:
        os.close(descriptor)


def publish(
    temp_path: Path,
    temp_identity: tuple[int, int],
    target: OutputTarget,
    force: bool,
    root: Path,
    parent_identity: tuple[int, int],
) -> None:
    revalidate_output_target(root, target, parent_identity, force)
    temp_info = lstat_or_none(temp_path)
    if (
        temp_info is None
        or is_reparse_stat(temp_info)
        or not stat.S_ISREG(temp_info.st_mode)
        or (temp_info.st_dev, temp_info.st_ino) != temp_identity
    ):
        raise AtlasBuildError("PDF temporary file identity changed at publication")
    try:
        if force:
            os.replace(temp_path, target.path)
        else:
            os.link(temp_path, target.path, follow_symlinks=False)
    except FileExistsError as error:
        raise AtlasBuildError(
            f"output appeared during no-clobber publication: {target.path}"
        ) from error
    except OSError as error:
        operation = "atomic replacement" if force else "atomic no-clobber hard link"
        raise AtlasBuildError(f"filesystem could not publish the PDF using {operation}: {error}") from error
    fsync_directory_best_effort(target.path.parent)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Build the source-first Voxel Native Codex Engineering Atlas. "
            "The artifact carries no runtime release verdict and keeps its runtime gallery pending."
        )
    )
    parser.add_argument(
        "--output",
        default=str(DEFAULT_OUTPUT),
        help=(
            "PDF destination under repository output/pdf or tmp "
            f"(default: {DEFAULT_OUTPUT.as_posix()})"
        ),
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Explicitly replace an existing output using atomic same-directory publication.",
    )
    parser.add_argument(
        "--no-clobber",
        action="store_true",
        help="Explicitly request the default no-clobber behavior.",
    )
    parser.add_argument(
        "--check-only",
        action="store_true",
        help="Validate source contracts, SVG assets, dependencies, and destination without writing a PDF.",
    )
    parser.add_argument(
        "--verify-determinism",
        action="store_true",
        help="Build twice in memory and require byte-identical PDFs before publication.",
    )
    parser.add_argument(
        "--validate-release",
        action="store_true",
        help=(
            "Read-only validation of the single canonical release PDF under "
            "docs/releases/technical-preview."
        ),
    )
    args = parser.parse_args(argv)
    if args.force and args.no_clobber:
        parser.error("--force and --no-clobber are mutually exclusive")
    if args.check_only and args.verify_determinism:
        parser.error("--check-only and --verify-determinism are mutually exclusive")
    if args.validate_release:
        conflicting = [
            flag
            for enabled, flag in (
                (args.force, "--force"),
                (args.no_clobber, "--no-clobber"),
                (args.check_only, "--check-only"),
                (args.verify_determinism, "--verify-determinism"),
            )
            if enabled
        ]
        if conflicting:
            parser.error(
                "--validate-release is mutually exclusive with " + ", ".join(conflicting)
            )
        if Path(args.output) != DEFAULT_OUTPUT:
            parser.error("--validate-release rejects a nondefault --output")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    root = repository_root()
    try:
        files, fingerprint = validate_inputs(root, _ATLAS_BOUND_BUILDER_BYTES)
        if args.validate_release:
            dependencies = load_pdf_dependencies()
            validate_svg_snapshots(files, dependencies["svg2rlg"])
            identity = compute_atlas_document_identity(
                files,
                fingerprint,
                dependencies["toolchain_identity"],
            )
            release_path, release_data = read_canonical_release_pdf(root)
            page_count = validate_built_pdf(
                release_data,
                dependencies["PdfReader"],
                dependencies["TextStringObject"],
                identity,
            )
            release_sha = hashlib.sha256(release_data).hexdigest()
            print(
                "atlas release valid: "
                f"path {release_path}; sha256 {release_sha}; "
                f"fingerprint {identity.fingerprint}; builder sha256 {identity.builder_sha}; "
                f"pages {page_count}"
            )
            return 0

        target = validate_output(root, args.output, args.force, check_only=args.check_only)
        dependencies = load_pdf_dependencies()
        if args.check_only:
            validate_svg_snapshots(files, dependencies["svg2rlg"])
            print(
                "atlas inputs valid: "
                f"{len(files)} immutable files; fingerprint {fingerprint}; output {target.path}"
            )
            return 0

        built = build_pdf_bytes(files, fingerprint, dependencies)
        if args.verify_determinism:
            second = build_pdf_bytes(files, fingerprint, dependencies)
            if second.data != built.data or second.sha256 != built.sha256:
                raise AtlasBuildError("two in-memory atlas builds were not byte-identical")

        parent_identity = prepare_output_parent(root, target)
        temp_path = write_validated_temp(target, built)
        published = False
        cleanup_error: OSError | None = None
        try:
            temp_identity = verify_temp_bytes(temp_path, built)
            publish(temp_path, temp_identity, target, args.force, root, parent_identity)
            published = True
        finally:
            try:
                temp_path.unlink(missing_ok=True)
            except OSError as error:
                cleanup_error = error
        if cleanup_error is not None:
            if not published:
                raise AtlasBuildError(
                    f"could not clean the unpublished PDF temporary file: {cleanup_error}"
                ) from cleanup_error
            print(
                f"atlas build warning: published output is valid but temporary cleanup failed: {cleanup_error}",
                file=sys.stderr,
            )
        print(
            f"built {target.path} ({len(built.data)} bytes, "
            f"{EXPECTED_PAGE_COUNT} pages, sha256 {built.sha256})"
        )
        print("runtime gallery pending; no runtime release verdict is encoded")
        return 0
    except AtlasBuildError as error:
        print(f"atlas build failed: {error}", file=sys.stderr)
        return 2
    except OSError as error:
        print(f"atlas build failed with a filesystem error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
