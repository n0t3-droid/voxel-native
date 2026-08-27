#!/usr/bin/env python3
"""Fail-closed four-arm analyzer for the L0 height diagnostic.

This tool consumes the two Natural and two Astral ``LodProvenanceV1`` QA
runs produced by one release executable.  It validates their report identity,
streaming health, matched camera ledger, and topology before measuring the
largest 8-connected component of the exact L0 red mask::

    (R > 200) & (G < 10) & (B < 30)

The result is diagnostic-only.  A successful automated result still requires
the human visual inspection in stop test 3; it is never canonical evidence.

This experiment deliberately fixes the physical viewport at 1920x1080.  Each
report must also use the Windows QA harness's single canonical screenshot path
form, ``<run-parent>\\<run-directory>\\<shot-name>``.  Absolute paths, rooted
paths, alternate separators, and additional prefixes are invalid evidence.

Runtime dependencies: Pillow, NumPy, and SciPy.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import io
import json
import math
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import secrets
import stat
import sys
from dataclasses import dataclass
from typing import Any, Iterable, Sequence

try:
    import numpy as np
    from PIL import Image, UnidentifiedImageError
    from scipy import ndimage
except ImportError as error:  # pragma: no cover - exercised by the CLI host
    raise SystemExit(
        "analyze_l0_provenance.py requires Pillow, NumPy, and SciPy"
    ) from error


ANALYZER_SCHEMA_VERSION = "voxel-native-l0-provenance-analysis-v1"
EVIDENCE_DISPOSITION = "diagnostic-only-non-publishable"

POINT_SCHEMA = "2.6.0-diagnostic-lod-provenance-v1"
CANDIDATE_SCHEMA = (
    "2.6.0-diagnostic-l0-cardinal-trimmed-8-v1-lod-provenance-v1"
)
LEGACY_POINT_SCHEMA = "2.5.0-diagnostic-lod-provenance-v1"
LEGACY_CANDIDATE_SCHEMA = (
    "2.5.0-diagnostic-l0-cardinal-trimmed-8-v1-lod-provenance-v1"
)
POINT_DISPOSITION = "diagnostic-lod-provenance-only-non-publishable"
CANDIDATE_DISPOSITION = (
    "diagnostic-l0-height-and-lod-provenance-only-non-publishable"
)

POINT_MODE = "Point16V1"
CANDIDATE_MODE = "CardinalTrimmed8V1"
SURFACE_MODE = "LodProvenanceV1"
DIAGNOSTIC_SCHEMAS_BY_MODE = {
    POINT_MODE: {
        "2.6.0": POINT_SCHEMA,
        "2.5.0": LEGACY_POINT_SCHEMA,
    },
    CANDIDATE_MODE: {
        "2.6.0": CANDIDATE_SCHEMA,
        "2.5.0": LEGACY_CANDIDATE_SCHEMA,
    },
}
EXPECTED_WORLD_SEED = 12_345
EXPECTED_SCENERY = "Lush"
EXPECTED_TERRAIN_GRAMMAR = "V3"
EXPECTED_BUILD_PROFILE = "release"
EXPECTED_VIEWPORT = (1_920, 1_080)

MASK_RED_MIN_EXCLUSIVE = 200
MASK_GREEN_MAX_EXCLUSIVE = 10
MASK_BLUE_MAX_EXCLUSIVE = 30
MAX_CANDIDATE_TO_BASELINE = 0.50
MAX_CANDIDATE_VIEWPORT_OCCUPANCY = 0.05

# Captures happen on the first rendered frame at or after a scheduled route
# time.  Half a metre and half a degree tightly bound one-frame scheduling
# jitter without allowing a materially different camera to be paired.
MAX_PAIRED_POSITION_DELTA_METRES = 0.50
MAX_PAIRED_ROTATION_DELTA_DEGREES = 0.50
MAX_PAIRED_CAPTURE_TIME_DELTA_SECONDS = 0.001

MAX_RUN_FILES = 128
MAX_RUN_TOTAL_BYTES = 512 * 1024 * 1024
MAX_REPORT_BYTES = 2 * 1024 * 1024
MAX_IMAGE_BYTES = 64 * 1024 * 1024
MAX_CAPTURE_COUNT = 64
MAX_IMAGE_DIMENSION = 8_192
MAX_IMAGE_PIXELS = 16_777_216
MAX_RON_DEPTH = 96
MAX_RON_NODES = 100_000
MAX_RON_STRING_CHARS = 16_384
PROTECTED_OUTPUT_COMPONENTS = frozenset({"saves", "qa_runs", "agent_runs"})

_SHA256_TOKEN_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
_GIT_SHA_RE = re.compile(r"[0-9a-f]{7,64}\Z")
_PLAN_HASH_RE = re.compile(r"[0-9a-f]{16}\Z")
_SHOT_NAME_RE = re.compile(r"shot_(\d{4})_([a-z0-9][a-z0-9_-]*)\.png\Z")
_NUMBER_RE = re.compile(
    r"[+-]?(?:\d(?:_?\d)*(?:\.\d(?:_?\d)*)?(?:[eE][+-]?\d(?:_?\d)*)?)"
)
_IDENTIFIER_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


class AnalysisError(ValueError):
    """The inputs cannot support a fail-closed diagnostic decision."""


class RonParseError(AnalysisError):
    """A report is outside the bounded non-executing RON subset."""


class RonParser:
    """Parse the serde-generated QA-report subset of RON without execution."""

    def __init__(self, text: str) -> None:
        self.text = text
        self.pos = 0
        self.nodes = 0

    def parse(self) -> Any:
        value = self._value(0)
        self._skip_whitespace()
        if self.pos != len(self.text):
            self._fail("unexpected trailing input")
        return value

    def _value(self, depth: int) -> Any:
        if depth > MAX_RON_DEPTH:
            self._fail("maximum nesting depth exceeded")
        self.nodes += 1
        if self.nodes > MAX_RON_NODES:
            self._fail("maximum parsed node count exceeded")
        self._skip_whitespace()
        if self.pos >= len(self.text):
            self._fail("expected value")

        char = self.text[self.pos]
        if char == '"':
            return self._string()
        if char == "(":
            return self._paren(depth + 1)
        if char == "[":
            return self._list(depth + 1)
        if char in "+-." or char.isdigit():
            return self._number()
        if char.isalpha() or char == "_":
            return self._identifier_value(depth + 1)
        self._fail(f"unsupported value starting with {char!r}")

    def _paren(self, depth: int) -> Any:
        self._expect("(")
        self._skip_whitespace()
        if self._take(")"):
            return []

        saved = self.pos
        field_name = self._try_field_name()
        if field_name is not None:
            self._skip_whitespace()
        is_struct = field_name is not None and self._take(":")
        self.pos = saved

        if is_struct:
            result: dict[str, Any] = {}
            while True:
                name = self._field_name()
                self._skip_whitespace()
                self._expect(":")
                if name in result:
                    self._fail(f"duplicate field {name!r}")
                result[name] = self._value(depth)
                self._skip_whitespace()
                if self._take(")"):
                    return result
                self._expect(",")
                self._skip_whitespace()
                if self._take(")"):
                    return result

        values: list[Any] = []
        while True:
            values.append(self._value(depth))
            self._skip_whitespace()
            if self._take(")"):
                return values
            self._expect(",")
            self._skip_whitespace()
            if self._take(")"):
                return values

    def _list(self, depth: int) -> list[Any]:
        self._expect("[")
        result: list[Any] = []
        self._skip_whitespace()
        if self._take("]"):
            return result
        while True:
            result.append(self._value(depth))
            self._skip_whitespace()
            if self._take("]"):
                return result
            self._expect(",")
            self._skip_whitespace()
            if self._take("]"):
                return result

    def _identifier_value(self, depth: int) -> Any:
        identifier = self._identifier()
        if identifier == "true":
            return True
        if identifier == "false":
            return False
        if identifier == "None":
            return None
        if identifier.lower() in {"nan", "inf"}:
            self._fail("non-finite numeric value is forbidden")

        self._skip_whitespace()
        if not self._take("("):
            return identifier
        self._skip_whitespace()
        values: list[Any] = []
        if not self._take(")"):
            while True:
                values.append(self._value(depth))
                self._skip_whitespace()
                if self._take(")"):
                    break
                self._expect(",")
                self._skip_whitespace()
                if self._take(")"):
                    break
        if identifier == "Some":
            if len(values) != 1:
                self._fail("Some requires exactly one value")
            return values[0]
        return {"__ron_variant__": identifier, "value": values}

    def _string(self) -> str:
        self._expect('"')
        result: list[str] = []
        while self.pos < len(self.text):
            char = self.text[self.pos]
            self.pos += 1
            if char == '"':
                value = "".join(result)
                if len(value) > MAX_RON_STRING_CHARS:
                    self._fail("maximum string length exceeded")
                return value
            if char != "\\":
                if ord(char) < 0x20:
                    self._fail("literal control character in string")
                result.append(char)
                continue
            if self.pos >= len(self.text):
                self._fail("unterminated string escape")
            escape = self.text[self.pos]
            self.pos += 1
            simple = {
                '"': '"',
                "\\": "\\",
                "n": "\n",
                "r": "\r",
                "t": "\t",
                "0": "\0",
            }
            if escape in simple:
                result.append(simple[escape])
            elif escape == "x":
                digits = self.text[self.pos : self.pos + 2]
                if len(digits) != 2 or not all(
                    character in "0123456789abcdefABCDEF" for character in digits
                ):
                    self._fail("invalid hexadecimal string escape")
                self.pos += 2
                result.append(chr(int(digits, 16)))
            elif escape == "u":
                self._expect("{")
                end = self.text.find("}", self.pos)
                if end < 0:
                    self._fail("unterminated Unicode string escape")
                digits = self.text[self.pos : end]
                if not 1 <= len(digits) <= 6 or not all(
                    character in "0123456789abcdefABCDEF" for character in digits
                ):
                    self._fail("invalid Unicode string escape")
                self.pos = end + 1
                result.append(chr(int(digits, 16)))
            else:
                self._fail(f"unsupported string escape \\{escape}")
        self._fail("unterminated string")

    def _number(self) -> int | float:
        match = _NUMBER_RE.match(self.text, self.pos)
        if match is None:
            self._fail("invalid number")
        token = match.group(0)
        self.pos = match.end()
        clean = token.replace("_", "")
        try:
            value: int | float
            if any(marker in clean for marker in ".eE"):
                value = float(clean)
                if not math.isfinite(value):
                    self._fail("non-finite numeric value is forbidden")
            else:
                value = int(clean, 10)
            return value
        except ValueError as error:
            raise RonParseError(
                f"invalid number {token!r} at offset {self.pos}"
            ) from error

    def _field_name(self) -> str:
        name = self._try_field_name()
        if name is None:
            self._fail("expected field name")
        return name

    def _try_field_name(self) -> str | None:
        self._skip_whitespace()
        match = _IDENTIFIER_RE.match(self.text, self.pos)
        if match is None:
            return None
        self.pos = match.end()
        return match.group(0)

    def _identifier(self) -> str:
        match = _IDENTIFIER_RE.match(self.text, self.pos)
        if match is None:
            self._fail("expected identifier")
        self.pos = match.end()
        return match.group(0)

    def _skip_whitespace(self) -> None:
        while self.pos < len(self.text) and self.text[self.pos].isspace():
            self.pos += 1
        # serde output contains no comments.  Rejecting them keeps the parser's
        # accepted language small and prevents hidden duplicate fields.
        if self.text.startswith(("//", "/*"), self.pos):
            self._fail("comments are forbidden in evidence reports")

    def _expect(self, token: str) -> None:
        self._skip_whitespace()
        if not self.text.startswith(token, self.pos):
            self._fail(f"expected {token!r}")
        self.pos += len(token)

    def _take(self, token: str) -> bool:
        if self.text.startswith(token, self.pos):
            self.pos += len(token)
            return True
        return False

    def _fail(self, message: str) -> None:
        raise RonParseError(f"{message} at offset {self.pos}")


@dataclass(frozen=True)
class ArmSpec:
    key: str
    profile_label: str
    world_profile: str
    route_focus: str
    mode: str
    schema: str
    disposition: str


ARM_SPECS = (
    ArmSpec(
        "natural_point",
        "Natural",
        "Natural",
        "river",
        POINT_MODE,
        POINT_SCHEMA,
        POINT_DISPOSITION,
    ),
    ArmSpec(
        "natural_cardinal",
        "Natural",
        "Natural",
        "river",
        CANDIDATE_MODE,
        CANDIDATE_SCHEMA,
        CANDIDATE_DISPOSITION,
    ),
    ArmSpec(
        "astral_point",
        "Astral",
        "AstralFrontier",
        "lava",
        POINT_MODE,
        POINT_SCHEMA,
        POINT_DISPOSITION,
    ),
    ArmSpec(
        "astral_cardinal",
        "Astral",
        "AstralFrontier",
        "lava",
        CANDIDATE_MODE,
        CANDIDATE_SCHEMA,
        CANDIDATE_DISPOSITION,
    ),
)


@dataclass(frozen=True)
class FileSnapshot:
    path: Path
    sha256: str
    size_bytes: int
    device: int
    inode: int
    mtime_ns: int


@dataclass(frozen=True)
class ImageEvidence:
    capture_index: int
    screenshot_name: str
    scheduled_capture_seconds: float
    translation: tuple[float, float, float]
    rotation: tuple[float, float, float, float]
    width: int
    height: int
    viewport_pixels: int
    wall_pixels: int
    occupancy: float
    snapshot: FileSnapshot


@dataclass
class RunEvidence:
    spec: ArmSpec
    run_dir: Path
    report: dict[str, Any]
    report_snapshot: FileSnapshot
    images: list[ImageEvidence]
    identity: dict[str, Any]
    viewport: dict[str, Any]
    streaming: dict[str, Any]
    route_plan_hash: str
    route_variant_index: int
    report_schema_generation: str


def _is_bool(value: Any) -> bool:
    return isinstance(value, bool)


def _require_dict(value: Any, context: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise AnalysisError(f"{context} must be a named-field structure")
    return value


def _require_list(value: Any, context: str) -> list[Any]:
    if not isinstance(value, list):
        raise AnalysisError(f"{context} must be a sequence")
    return value


def _field(mapping: dict[str, Any], name: str, context: str) -> Any:
    if name not in mapping:
        raise AnalysisError(f"{context}.{name} is missing")
    return mapping[name]


def _string(mapping: dict[str, Any], name: str, context: str) -> str:
    value = _field(mapping, name, context)
    if not isinstance(value, str) or not value:
        raise AnalysisError(f"{context}.{name} must be a non-empty string")
    return value


def _boolean(mapping: dict[str, Any], name: str, context: str) -> bool:
    value = _field(mapping, name, context)
    if not _is_bool(value):
        raise AnalysisError(f"{context}.{name} must be a boolean")
    return value


def _integer(mapping: dict[str, Any], name: str, context: str) -> int:
    value = _field(mapping, name, context)
    if not isinstance(value, int) or _is_bool(value):
        raise AnalysisError(f"{context}.{name} must be an integer")
    return value


def _number(mapping: dict[str, Any], name: str, context: str) -> float:
    value = _field(mapping, name, context)
    if not isinstance(value, (int, float)) or _is_bool(value):
        raise AnalysisError(f"{context}.{name} must be numeric")
    result = float(value)
    if not math.isfinite(result):
        raise AnalysisError(f"{context}.{name} must be finite")
    return result


def _expect_equal(actual: Any, expected: Any, context: str) -> None:
    if actual != expected:
        raise AnalysisError(f"{context} is {actual!r}; expected {expected!r}")


def _expect_true(mapping: dict[str, Any], name: str, context: str) -> None:
    _expect_equal(_boolean(mapping, name, context), True, f"{context}.{name}")


def _expect_false(mapping: dict[str, Any], name: str, context: str) -> None:
    _expect_equal(_boolean(mapping, name, context), False, f"{context}.{name}")


def _expect_zero(mapping: dict[str, Any], name: str, context: str) -> None:
    _expect_equal(_integer(mapping, name, context), 0, f"{context}.{name}")


def _reject_reparse_or_symlink(path: Path, context: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise AnalysisError(f"{context} could not be inspected: {error}") from error
    reparse_flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    file_attributes = getattr(metadata, "st_file_attributes", 0)
    if stat.S_ISLNK(metadata.st_mode) or (reparse_flag and file_attributes & reparse_flag):
        raise AnalysisError(f"{context} may not be a symlink or reparse point")


def _reject_symlink_components(path: Path, context: str) -> None:
    absolute = Path(os.path.abspath(path))
    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current /= part
        if current.exists() or current.is_symlink():
            _reject_reparse_or_symlink(current, context)


def _path_comparison_key(path: Path) -> str:
    """Normalize case and Windows extended namespaces for identity checks."""

    value = os.path.normpath(str(path))
    if os.name == "nt":
        if value.casefold().startswith("\\\\?\\unc\\"):
            value = "\\\\" + value[8:]
        elif value.startswith("\\\\?\\"):
            value = value[4:]
    return os.path.normcase(value)


def _validate_run_dir(raw_path: str | os.PathLike[str], context: str) -> Path:
    lexical = Path(os.path.abspath(raw_path))
    _reject_symlink_components(lexical, context)
    try:
        resolved = lexical.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise AnalysisError(f"{context} could not be resolved: {error}") from error
    if _path_comparison_key(lexical) != _path_comparison_key(resolved):
        raise AnalysisError(f"{context} resolves through an alias or symlink")
    if not resolved.is_dir():
        raise AnalysisError(f"{context} is not a directory")

    try:
        entries = list(resolved.iterdir())
    except OSError as error:
        raise AnalysisError(f"{context} could not be enumerated: {error}") from error
    if len(entries) > MAX_RUN_FILES:
        raise AnalysisError(f"{context} exceeds the {MAX_RUN_FILES}-file cap")
    total_bytes = 0
    for entry in entries:
        _reject_reparse_or_symlink(entry, f"{context} entry {entry.name!r}")
        try:
            metadata = entry.stat(follow_symlinks=False)
        except OSError as error:
            raise AnalysisError(f"{context} entry {entry.name!r} is unreadable: {error}") from error
        if not stat.S_ISREG(metadata.st_mode):
            raise AnalysisError(f"{context} entry {entry.name!r} is not a regular file")
        if metadata.st_size < 0 or metadata.st_size > MAX_IMAGE_BYTES:
            raise AnalysisError(f"{context} entry {entry.name!r} exceeds the per-file cap")
        total_bytes += metadata.st_size
        if total_bytes > MAX_RUN_TOTAL_BYTES:
            raise AnalysisError(f"{context} exceeds the total byte cap")
    return resolved


def _safe_read(path: Path, byte_cap: int, context: str) -> tuple[bytes, FileSnapshot]:
    _reject_reparse_or_symlink(path, context)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise AnalysisError(f"{context} could not be opened safely: {error}") from error
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise AnalysisError(f"{context} is not a regular file")
        if metadata.st_size < 0 or metadata.st_size > byte_cap:
            raise AnalysisError(f"{context} exceeds the {byte_cap}-byte cap")
        chunks: list[bytes] = []
        remaining = metadata.st_size
        while remaining:
            chunk = os.read(descriptor, min(remaining, 1024 * 1024))
            if not chunk:
                raise AnalysisError(f"{context} changed while it was being read")
            chunks.append(chunk)
            remaining -= len(chunk)
        if os.read(descriptor, 1):
            raise AnalysisError(f"{context} grew while it was being read")
        payload = b"".join(chunks)
    finally:
        os.close(descriptor)

    try:
        after = path.stat(follow_symlinks=False)
    except OSError as error:
        raise AnalysisError(f"{context} disappeared after reading: {error}") from error
    identity_before = (metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns)
    identity_after = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    if identity_before != identity_after:
        raise AnalysisError(f"{context} changed while it was being read")
    return payload, FileSnapshot(
        path=path,
        sha256=hashlib.sha256(payload).hexdigest(),
        size_bytes=len(payload),
        device=metadata.st_dev,
        inode=metadata.st_ino,
        mtime_ns=metadata.st_mtime_ns,
    )


def _verify_snapshot_unchanged(snapshot: FileSnapshot, byte_cap: int, context: str) -> None:
    _, current = _safe_read(snapshot.path, byte_cap, context)
    if (
        current.sha256 != snapshot.sha256
        or current.size_bytes != snapshot.size_bytes
        or current.device != snapshot.device
        or current.inode != snapshot.inode
        or current.mtime_ns != snapshot.mtime_ns
    ):
        raise AnalysisError(f"{context} hash or file identity changed during analysis")


def _load_report(run_dir: Path, spec: ArmSpec) -> tuple[dict[str, Any], FileSnapshot]:
    report_path = run_dir / "report.ron"
    payload, snapshot = _safe_read(report_path, MAX_REPORT_BYTES, f"{spec.key} report")
    try:
        text = payload.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise AnalysisError(f"{spec.key} report is not strict UTF-8") from error
    if text.startswith("\ufeff"):
        raise AnalysisError(f"{spec.key} report may not contain a UTF-8 BOM")
    report = _require_dict(RonParser(text).parse(), f"{spec.key} report")
    return report, snapshot


def _sequence_of_ints(value: Any, length: int, context: str) -> tuple[int, ...]:
    values = _require_list(value, context)
    if len(values) != length or any(
        not isinstance(item, int) or _is_bool(item) or item < 0 for item in values
    ):
        raise AnalysisError(f"{context} must contain {length} non-negative integers")
    return tuple(values)


def _validate_identity(
    report: dict[str, Any], spec: ArmSpec
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any], str, int, str]:
    context = spec.key
    report_schema = _string(report, "qa_report_schema_version", context)
    schema_generation = next(
        (
            generation
            for generation, expected_schema in DIAGNOSTIC_SCHEMAS_BY_MODE[spec.mode].items()
            if report_schema == expected_schema
        ),
        None,
    )
    if schema_generation is None:
        expected = sorted(DIAGNOSTIC_SCHEMAS_BY_MODE[spec.mode].values())
        raise AnalysisError(
            f"{context}.qa_report_schema_version is {report_schema!r}, expected one of {expected!r}"
        )
    _expect_equal(
        _string(report, "evidence_disposition", context),
        spec.disposition,
        f"{context}.evidence_disposition",
    )

    identity = _require_dict(_field(report, "run_identity", context), f"{context}.run_identity")
    identity_context = f"{context}.run_identity"
    _expect_equal(
        _string(identity, "build_profile", identity_context),
        EXPECTED_BUILD_PROFILE,
        f"{identity_context}.build_profile",
    )
    _string(identity, "package_version", identity_context)
    _string(identity, "instance_label", identity_context)
    _string(identity, "world_name", identity_context)
    _expect_equal(
        _integer(identity, "world_seed", identity_context),
        EXPECTED_WORLD_SEED,
        f"{identity_context}.world_seed",
    )
    _expect_equal(
        _string(identity, "world_profile", identity_context),
        spec.world_profile,
        f"{identity_context}.world_profile",
    )
    _expect_equal(
        _string(identity, "scenery_quality", identity_context),
        EXPECTED_SCENERY,
        f"{identity_context}.scenery_quality",
    )
    _expect_equal(
        _string(identity, "terrain_grammar", identity_context),
        EXPECTED_TERRAIN_GRAMMAR,
        f"{identity_context}.terrain_grammar",
    )
    git_sha = _string(identity, "git_sha", identity_context)
    if not _GIT_SHA_RE.fullmatch(git_sha):
        raise AnalysisError(f"{identity_context}.git_sha is not a bounded hexadecimal SHA")
    _boolean(identity, "git_dirty", identity_context)
    for name in ("source_fingerprint", "executable_hash"):
        token = _string(identity, name, identity_context)
        if not _SHA256_TOKEN_RE.fullmatch(token):
            raise AnalysisError(f"{identity_context}.{name} is not a sha256 token")
    toolchain = _string(identity, "toolchain", identity_context)
    if "host: x86_64-pc-windows-msvc" not in toolchain:
        raise AnalysisError(
            f"{identity_context}.toolchain is not the required x86_64-pc-windows-msvc "
            "cache-accounting host contract"
        )
    _string(identity, "hardware", identity_context)

    _expect_equal(
        _string(report, "world_edit_store_status", context),
        "compatible",
        f"{context}.world_edit_store_status",
    )
    _expect_true(report, "world_edit_store_compatible", context)
    _expect_equal(
        _integer(report, "world_edit_store_seed", context),
        EXPECTED_WORLD_SEED,
        f"{context}.world_edit_store_seed",
    )
    _expect_equal(
        _string(report, "world_edit_store_profile", context),
        spec.world_profile,
        f"{context}.world_edit_store_profile",
    )
    _expect_equal(
        _string(report, "world_edit_store_scenery_quality", context),
        EXPECTED_SCENERY,
        f"{context}.world_edit_store_scenery_quality",
    )
    _expect_equal(
        _string(report, "world_edit_store_terrain_grammar", context),
        EXPECTED_TERRAIN_GRAMMAR,
        f"{context}.world_edit_store_terrain_grammar",
    )
    block_reason = _field(report, "world_edit_store_block_reason_code", context)
    if block_reason is not None:
        raise AnalysisError(f"{context}.world_edit_store_block_reason_code must be None")
    _expect_zero(report, "world_edit_store_edited_chunks", context)

    viewport = _require_dict(_field(report, "viewport", context), f"{context}.viewport")
    viewport_context = f"{context}.viewport"
    width = _integer(viewport, "physical_width", viewport_context)
    height = _integer(viewport, "physical_height", viewport_context)
    if (width, height) != EXPECTED_VIEWPORT:
        raise AnalysisError(
            f"{viewport_context} physical size is {width}x{height}; the fixed "
            f"diagnostic contract requires {EXPECTED_VIEWPORT[0]}x{EXPECTED_VIEWPORT[1]}"
        )
    if not 1 <= width <= MAX_IMAGE_DIMENSION or not 1 <= height <= MAX_IMAGE_DIMENSION:
        raise AnalysisError(f"{viewport_context} dimensions exceed the hard cap")
    if width * height > MAX_IMAGE_PIXELS:
        raise AnalysisError(f"{viewport_context} pixel count exceeds the hard cap")
    logical_width = _number(viewport, "logical_width", viewport_context)
    logical_height = _number(viewport, "logical_height", viewport_context)
    scale = _number(viewport, "scale_factor", viewport_context)
    dpi = _number(viewport, "dpi_percent", viewport_context)
    if logical_width <= 0 or logical_height <= 0 or not 0.25 <= scale <= 8.0:
        raise AnalysisError(f"{viewport_context} contains an invalid logical size or scale")
    if abs(logical_width * scale - width) > 1.0 or abs(logical_height * scale - height) > 1.0:
        raise AnalysisError(f"{viewport_context} logical and physical sizes disagree")
    if schema_generation == "2.6.0":
        base_scale = _number(viewport, "base_scale_factor", viewport_context)
        if not 0.25 <= base_scale <= 8.0:
            raise AnalysisError(f"{viewport_context}.base_scale_factor is invalid")
        if abs(dpi - base_scale * 100.0) > 0.01:
            raise AnalysisError(
                f"{viewport_context}.dpi_percent disagrees with base_scale_factor"
            )
    else:
        if "base_scale_factor" in viewport:
            raise AnalysisError(
                f"{viewport_context}.base_scale_factor is not part of the exact legacy 2.5 contract"
            )
        if abs(dpi - scale * 100.0) > 0.01:
            raise AnalysisError(
                f"{viewport_context}.dpi_percent disagrees with legacy scale_factor"
            )

    _expect_equal(
        _string(report, "requested_route_focus", context),
        spec.route_focus,
        f"{context}.requested_route_focus",
    )
    _expect_equal(
        _string(report, "resolved_route_focus", context),
        spec.route_focus,
        f"{context}.resolved_route_focus",
    )
    _expect_true(report, "route_focus_available", context)
    if _field(report, "route_focus_unavailable_reason", context) is not None:
        raise AnalysisError(f"{context}.route_focus_unavailable_reason must be None")
    _expect_false(report, "route_focus_search_cap_exhausted", context)
    _expect_true(report, "camera_route_preflight_applicable", context)
    _expect_equal(
        _string(report, "camera_route_policy", context),
        "preflight-v1",
        f"{context}.camera_route_policy",
    )
    _expect_true(report, "camera_route_available", context)
    if _field(report, "camera_route_unavailable_reason", context) is not None:
        raise AnalysisError(f"{context}.camera_route_unavailable_reason must be None")
    _expect_false(report, "camera_route_work_cap_exhausted", context)
    plan_hash = _string(report, "camera_route_plan_hash", context)
    if not _PLAN_HASH_RE.fullmatch(plan_hash):
        raise AnalysisError(f"{context}.camera_route_plan_hash must be 16 lowercase hex digits")
    variant_index = _integer(report, "camera_route_variant_index", context)
    variant_count = _integer(report, "camera_route_variant_count", context)
    if not 0 <= variant_index < variant_count or variant_count != 8:
        raise AnalysisError(f"{context} camera route variant is out of bounds")
    selected_samples = _integer(report, "camera_route_selected_clear_samples", context)
    validation_samples = _integer(report, "camera_route_validation_samples", context)
    if selected_samples != 16 or validation_samples != 16:
        raise AnalysisError(f"{context} camera route was not fully preflighted")
    voxel_queries = _integer(report, "camera_route_voxel_queries", context)
    voxel_query_cap = _integer(report, "camera_route_voxel_query_cap", context)
    required_checks = _integer(report, "camera_route_required_chunk_checks", context)
    loaded_checks = _integer(report, "camera_route_loaded_chunk_checks", context)
    proven_air_checks = _integer(report, "camera_route_proven_air_chunk_checks", context)
    unloaded_checks = _integer(report, "camera_route_unloaded_chunk_checks", context)
    if voxel_query_cap != 153_600 or not 0 < voxel_queries <= voxel_query_cap:
        raise AnalysisError(f"{context} camera route query accounting is invalid")
    if (
        required_checks != voxel_queries
        or loaded_checks < 0
        or proven_air_checks < 0
        or loaded_checks + proven_air_checks + unloaded_checks != required_checks
    ):
        raise AnalysisError(f"{context} camera route chunk-check accounting is inconsistent")
    _expect_zero(report, "camera_route_unloaded_chunk_checks", context)
    minimum_clearance = _integer(report, "camera_route_minimum_clearance_voxels", context)
    if minimum_clearance <= 0:
        raise AnalysisError(f"{context}.camera_route_minimum_clearance_voxels must be positive")
    # These two counters include rejected preflight variants.  A later clear
    # variant is valid, so nonzero values are evidence of bounded rejection,
    # not evidence that the selected route is occluded.
    for name in (
        "camera_route_candidate_body_occlusions",
        "camera_route_candidate_los_occlusions",
    ):
        if _integer(report, name, context) < 0:
            raise AnalysisError(f"{context}.{name} may not be negative")
    duration = _number(report, "duration_seconds", context)
    requested_duration = _number(report, "requested_duration_seconds", context)
    if not 0 < duration <= 600.0 or abs(duration - requested_duration) > 0.001:
        raise AnalysisError(f"{context} route duration is invalid or incomplete")
    for name in ("pending_terrain", "pending_meshes", "dirty_chunks"):
        _expect_zero(report, name, context)
    if _integer(report, "loaded_chunks", context) <= 0 or _integer(
        report, "mesh_entities", context
    ) <= 0:
        raise AnalysisError(f"{context} near-field population did not settle")

    streaming = _require_dict(
        _field(report, "planetary_streaming", context),
        f"{context}.planetary_streaming",
    )
    _validate_streaming(streaming, spec)
    return (
        identity,
        viewport,
        streaming,
        plan_hash,
        variant_index,
        schema_generation,
    )


def _expected_l0_sampling_identity(
    spec: ArmSpec, cache_update: str, shift_x: int, shift_z: int
) -> tuple[int, int, int, int]:
    """Recompute the exact L0 center/half-plane queries and reuse population."""

    if not -(2**31) <= shift_x <= 2**31 - 1 or not -(2**31) <= shift_z <= 2**31 - 1:
        raise AnalysisError(f"{spec.key} L0 cache shift is outside the signed i32 contract")
    abs_x = abs(shift_x)
    abs_z = abs(shift_z)
    candidate = spec.mode == CANDIDATE_MODE

    if cache_update in ("Cold", "IncompatibleFallback"):
        if shift_x != 0 or shift_z != 0:
            raise AnalysisError(
                f"{spec.key} {cache_update} L0 cache update must have a zero shift"
            )
        return 4_225, 4_290 if candidate else 0, 4_290 if candidate else 0, 0
    if cache_update == "TeleportFallback":
        if (abs_x != 0 or abs_z != 0) and abs_x < 65 and abs_z < 65:
            raise AnalysisError(
                f"{spec.key} TeleportFallback is neither the unrepresentable-delta "
                "zero sentinel nor a shift crossing the 65-cell boundary"
            )
        return 4_225, 4_290 if candidate else 0, 4_290 if candidate else 0, 0
    if cache_update != "IncrementalStrip":
        raise AnalysisError(f"{spec.key} has unsupported last_l0_cache_update {cache_update!r}")
    if abs_x >= 65 or abs_z >= 65:
        raise AnalysisError(f"{spec.key} IncrementalStrip exceeds the 64-cell overlap boundary")

    center = 4_225 - (65 - abs_x) * (65 - abs_z)
    half_x = 0
    half_z = 0
    reused = 4_225 - center
    if candidate:
        half_x = 4_290 - (66 - abs_x) * (65 - abs_z)
        half_z = 4_290 - (65 - abs_x) * (66 - abs_z)
        reused += (4_290 - half_x) + (4_290 - half_z)
    return center, half_x, half_z, reused


def _validate_streaming(streaming: dict[str, Any], spec: ArmSpec) -> None:
    context = f"{spec.key}.planetary_streaming"
    _expect_true(streaming, "enabled", context)
    _expect_equal(_string(streaming, "profile", context), spec.world_profile, f"{context}.profile")
    for field_name in ("desired_terrain_grammar", "active_terrain_grammar"):
        _expect_equal(
            _string(streaming, field_name, context),
            EXPECTED_TERRAIN_GRAMMAR,
            f"{context}.{field_name}",
        )
    for field_name in (
        "desired_l0_height_mode",
        "active_l0_height_mode",
        "resident_l0_height_mode",
    ):
        _expect_equal(
            _string(streaming, field_name, context),
            spec.mode,
            f"{context}.{field_name}",
        )
    _expect_equal(_integer(streaming, "l0_probe_spacing_metres", context), 8, f"{context}.l0_probe_spacing_metres")
    _expect_equal(_integer(streaming, "budget_l0_height_queries", context), 12_805, f"{context}.budget_l0_height_queries")
    _expect_equal(_string(streaming, "surface_material_mode", context), SURFACE_MODE, f"{context}.surface_material_mode")
    _expect_equal(_string(streaming, "hydro_mode", context), "Disabled", f"{context}.hydro_mode")
    _expect_equal(_string(streaming, "semantic_cohort_mode", context), "Disabled", f"{context}.semantic_cohort_mode")

    for name in (
        "resident_observation_valid",
        "resident_fluid_observation_valid",
        "resident_fluid_kind_integrity_valid",
        "resident_semantic_cohort_observation_valid",
        "resident_semantic_cohort_payload_integrity_valid",
    ):
        _expect_true(streaming, name, context)
    for name in (
        "resident_entity_count_overflow",
        "resident_scheduler_mismatch",
        "resident_budget_exceeded",
        "resident_fluid_entity_count_overflow",
        "resident_fluid_scheduler_mismatch",
        "resident_fluid_budget_exceeded",
        "resident_semantic_cohort_entity_count_overflow",
        "resident_semantic_cohort_scheduler_mismatch",
        "resident_semantic_cohort_budget_exceeded",
        "build_in_flight",
    ):
        _expect_false(streaming, name, context)
    for name in (
        "resident_duplicate_levels",
        "resident_out_of_range_levels",
        "resident_observation_rejections",
        "resident_fluid_duplicate_slots",
        "resident_fluid_out_of_range_levels",
        "resident_fluid_observation_rejections",
        "resident_semantic_cohort_observation_rejections",
        "budget_rejections",
        "pending_rebuilds",
        "dirty_mask",
    ):
        _expect_zero(streaming, name, context)

    resident_entities = _integer(streaming, "resident_entities", context)
    scheduler_entities = _integer(streaming, "scheduler_resident_entities", context)
    budget_entities = _integer(streaming, "budget_entities", context)
    if resident_entities != scheduler_entities or resident_entities != 6 or budget_entities != 6:
        raise AnalysisError(f"{context} must contain exactly six agreeing terrain entities")

    scalar_pairs = (
        ("resident_vertices", "scheduler_resident_vertices", "budget_vertices"),
        ("resident_indices", "scheduler_resident_indices", "budget_indices"),
        ("resident_mesh_bytes", "scheduler_resident_mesh_bytes", "budget_mesh_bytes"),
    )
    for resident_name, scheduler_name, budget_name in scalar_pairs:
        resident = _integer(streaming, resident_name, context)
        scheduler = _integer(streaming, scheduler_name, context)
        budget = _integer(streaming, budget_name, context)
        if resident <= 0 or resident != scheduler or resident > budget:
            raise AnalysisError(f"{context}.{resident_name} disagrees with scheduler or budget")

    ring_vertices = _sequence_of_ints(_field(streaming, "ring_vertices", context), 6, f"{context}.ring_vertices")
    scheduler_ring_vertices = _sequence_of_ints(_field(streaming, "scheduler_ring_vertices", context), 6, f"{context}.scheduler_ring_vertices")
    ring_indices = _sequence_of_ints(_field(streaming, "ring_indices", context), 6, f"{context}.ring_indices")
    scheduler_ring_indices = _sequence_of_ints(_field(streaming, "scheduler_ring_indices", context), 6, f"{context}.scheduler_ring_indices")
    if ring_vertices != scheduler_ring_vertices or ring_indices != scheduler_ring_indices:
        raise AnalysisError(f"{context} per-ring topology disagrees with the scheduler")
    if sum(ring_vertices) != _integer(streaming, "resident_vertices", context):
        raise AnalysisError(f"{context}.ring_vertices do not sum to resident_vertices")
    if sum(ring_indices) != _integer(streaming, "resident_indices", context):
        raise AnalysisError(f"{context}.ring_indices do not sum to resident_indices")

    for prefix in ("resident", "scheduler_resident"):
        for suffix in (
            "fluid_entities",
            "fluid_vertices",
            "fluid_indices",
            "fluid_mesh_bytes",
            "semantic_cohort_entities",
            "semantic_cohort_vertices",
            "semantic_cohort_indices",
            "semantic_cohort_mesh_bytes",
            "semantic_cohort_count",
        ):
            _expect_zero(streaming, f"{prefix}_{suffix}", context)

    live_windows = _integer(streaming, "live_sample_cache_windows", context)
    peak_windows = _integer(streaming, "peak_live_sample_cache_windows", context)
    if live_windows != 6 or not 6 <= peak_windows <= 6:
        raise AnalysisError(f"{context} must report a complete six-window sample-cache population")
    live_bytes = _integer(streaming, "live_sample_cache_bytes", context)
    peak_bytes = _integer(streaming, "peak_live_sample_cache_bytes", context)
    budget_bytes = _integer(streaming, "budget_sample_cache_bytes", context)
    mode_cap = 228_822 if spec.mode == POINT_MODE else 263_142
    if live_bytes != mode_cap or peak_bytes != mode_cap:
        raise AnalysisError(
            f"{context} sample-cache bytes must equal the settled six-window "
            f"accounted population of {mode_cap} bytes"
        )
    if budget_bytes != 524_288 or peak_bytes > budget_bytes:
        raise AnalysisError(f"{context} sample-cache budget is invalid")

    center = _integer(streaming, "last_l0_center_queries", context)
    half_x = _integer(streaming, "last_l0_half_x_queries", context)
    half_z = _integer(streaming, "last_l0_half_z_queries", context)
    cache_update = _string(streaming, "last_l0_cache_update", context)
    cache_shift_x = _integer(streaming, "last_l0_cache_shift_x_cells", context)
    cache_shift_z = _integer(streaming, "last_l0_cache_shift_z_cells", context)
    reused_height_samples = _integer(
        streaming, "last_l0_reused_height_samples", context
    )
    trimmed = _integer(streaming, "last_l0_trimmed_vertices", context)
    trimmed_up = _integer(streaming, "last_l0_trimmed_up_vertices", context)
    trimmed_down = _integer(streaming, "last_l0_trimmed_down_vertices", context)
    max_adjustment = _number(streaming, "last_l0_max_abs_adjustment_metres", context)
    if min(center, half_x, half_z, trimmed, trimmed_up, trimmed_down) < 0:
        raise AnalysisError(f"{context} L0 query/effect counters may not be negative")
    if center > 4_225:
        raise AnalysisError(f"{context}.last_l0_center_queries exceeds the 4225-query plane cap")
    if half_x > 4_290:
        raise AnalysisError(f"{context}.last_l0_half_x_queries exceeds the 4290-query plane cap")
    if half_z > 4_290:
        raise AnalysisError(f"{context}.last_l0_half_z_queries exceeds the 4290-query plane cap")
    if center + half_x + half_z > 12_805:
        raise AnalysisError(f"{context} L0 query counters exceed the hard work cap")
    if trimmed > 3_721 or trimmed_up > 3_721 or trimmed_down > 3_721:
        raise AnalysisError(f"{context} L0 trim counters exceed the 3721-vertex lattice cap")
    if trimmed != trimmed_up + trimmed_down or max_adjustment < 0:
        raise AnalysisError(f"{context} L0 effect counters are internally inconsistent")
    if (trimmed == 0) != (max_adjustment == 0.0):
        raise AnalysisError(
            f"{context} L0 adjustment magnitude is inconsistent with its trimmed-vertex count"
        )
    if spec.mode == POINT_MODE and any((half_x, half_z, trimmed, max_adjustment)):
        raise AnalysisError(f"{context} point mode reports candidate-only work or effects")
    if spec.mode == CANDIDATE_MODE and not (
        (center == 0 and half_x == 0 and half_z == 0)
        or (center > 0 and half_x > 0 and half_z > 0)
    ):
        raise AnalysisError(
            f"{context} candidate L0 query planes have inconsistent zero/nonzero populations"
        )
    expected_sampling = _expected_l0_sampling_identity(
        spec, cache_update, cache_shift_x, cache_shift_z
    )
    actual_sampling = (center, half_x, half_z, reused_height_samples)
    if actual_sampling != expected_sampling:
        raise AnalysisError(
            f"{context} L0 query/reuse counters {actual_sampling!r} do not match "
            f"the exact {cache_update} cache-shift identity {expected_sampling!r}"
        )


def _observation_path(run_dir: Path, serialized_path: str, index: int, context: str) -> Path:
    if not serialized_path.isascii() or len(serialized_path) > 512:
        raise AnalysisError(f"{context} screenshot path is not bounded ASCII")
    windows_path = PureWindowsPath(serialized_path)
    posix_path = PurePosixPath(serialized_path)
    if (
        serialized_path.startswith(("/", "\\"))
        or windows_path.is_absolute()
        or bool(windows_path.drive)
        or bool(windows_path.root)
        or posix_path.is_absolute()
    ):
        raise AnalysisError(
            f"{context} screenshot path may not be absolute, rooted, drive-qualified, or UNC"
        )
    # Windows QA has one canonical serialized representation, relative to the
    # parent of the validated run collection.  Accepting only the basename or
    # comparing final components would allow arbitrary-prefix rebinding.
    parts = serialized_path.split("\\")
    if len(parts) != 3 or any(part in {"", ".", ".."} for part in parts):
        raise AnalysisError(
            f"{context} screenshot path is not the canonical three-component QA path"
        )
    filename = parts[2]
    match = _SHOT_NAME_RE.fullmatch(filename)
    if match is None or int(match.group(1)) != index:
        raise AnalysisError(f"{context} screenshot filename does not bind capture index {index}")
    canonical_path = "\\".join((run_dir.parent.name, run_dir.name, filename))
    if serialized_path != canonical_path:
        raise AnalysisError(
            f"{context} screenshot path is not the exact canonical report representation "
            f"{canonical_path!r}"
        )
    # Resolve and hash the exact serialized components instead of discarding
    # their prefix and silently rebinding a basename beneath run_dir.
    result = run_dir.parent.parent.joinpath(*parts)
    try:
        resolved = result.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise AnalysisError(f"{context} screenshot does not resolve: {error}") from error
    if resolved.parent != run_dir:
        raise AnalysisError(f"{context} screenshot escapes its run directory")
    _reject_reparse_or_symlink(resolved, f"{context} screenshot")
    return resolved


def _finite_vector(value: Any, length: int, context: str) -> tuple[float, ...]:
    values = _require_list(value, context)
    if len(values) != length:
        raise AnalysisError(f"{context} must contain exactly {length} values")
    result: list[float] = []
    for item in values:
        if not isinstance(item, (int, float)) or _is_bool(item) or not math.isfinite(float(item)):
            raise AnalysisError(f"{context} contains a non-finite or non-numeric value")
        result.append(float(item))
    return tuple(result)


def _measure_png(
    snapshot: FileSnapshot, expected_width: int, expected_height: int, context: str
) -> tuple[int, int, int, int, float]:
    payload, current = _safe_read(snapshot.path, MAX_IMAGE_BYTES, context)
    if current.sha256 != snapshot.sha256:
        raise AnalysisError(f"{context} image hash changed before decoding")
    try:
        with Image.open(io.BytesIO(payload)) as image:
            if image.format != "PNG":
                raise AnalysisError(f"{context} is not a PNG image")
            if getattr(image, "n_frames", 1) != 1:
                raise AnalysisError(f"{context} must be a single-frame PNG")
            width, height = image.size
            if (width, height) != (expected_width, expected_height):
                raise AnalysisError(
                    f"{context} is {width}x{height}; expected {expected_width}x{expected_height}"
                )
            if (
                width <= 0
                or height <= 0
                or width > MAX_IMAGE_DIMENSION
                or height > MAX_IMAGE_DIMENSION
                or width * height > MAX_IMAGE_PIXELS
            ):
                raise AnalysisError(f"{context} dimensions exceed the hard image cap")
            pixels = np.asarray(image.convert("RGB"), dtype=np.uint8)
    except (UnidentifiedImageError, OSError, ValueError) as error:
        raise AnalysisError(f"{context} could not be decoded safely: {error}") from error

    mask = (
        (pixels[:, :, 0] > MASK_RED_MIN_EXCLUSIVE)
        & (pixels[:, :, 1] < MASK_GREEN_MAX_EXCLUSIVE)
        & (pixels[:, :, 2] < MASK_BLUE_MAX_EXCLUSIVE)
    )
    labels, component_count = ndimage.label(
        mask, structure=np.ones((3, 3), dtype=np.uint8)
    )
    if component_count == 0:
        largest = 0
    else:
        populations = np.bincount(labels.reshape(-1))
        largest = int(populations[1:].max(initial=0))
    viewport_pixels = width * height
    return width, height, viewport_pixels, largest, largest / viewport_pixels


def _load_images(
    report: dict[str, Any], run_dir: Path, spec: ArmSpec, viewport: dict[str, Any]
) -> list[ImageEvidence]:
    context = spec.key
    _expect_true(report, "screenshot_observation_valid", context)
    _expect_false(report, "screenshot_observation_cap_exhausted", context)
    _expect_zero(report, "screenshot_observation_rejections", context)
    screenshots = _require_list(_field(report, "screenshots", context), f"{context}.screenshots")
    observations = _require_list(
        _field(report, "screenshot_observations", context),
        f"{context}.screenshot_observations",
    )
    count = _integer(report, "screenshot_observation_count", context)
    cap = _integer(report, "screenshot_observation_cap", context)
    if not 1 <= count <= MAX_CAPTURE_COUNT or count != len(screenshots) or count != len(observations):
        raise AnalysisError(f"{context} screenshot ledger has an invalid bounded count")
    if cap != 600:
        raise AnalysisError(f"{context}.screenshot_observation_cap must equal 600")
    _expect_equal(
        _integer(report, "screenshot_path_max_chars", context),
        512,
        f"{context}.screenshot_path_max_chars",
    )

    duration = _number(report, "duration_seconds", context)
    width = _integer(viewport, "physical_width", f"{context}.viewport")
    height = _integer(viewport, "physical_height", f"{context}.viewport")
    evidence: list[ImageEvidence] = []
    referenced_names: set[str] = set()
    previous_time = -math.inf
    for expected_index, raw_observation in enumerate(observations):
        observation = _require_dict(raw_observation, f"{context} observation {expected_index}")
        observation_context = f"{context}.screenshot_observations[{expected_index}]"
        index = _integer(observation, "capture_index", observation_context)
        if index != expected_index:
            raise AnalysisError(f"{observation_context}.capture_index is duplicate, missing, or unordered")
        serialized_path = _string(observation, "screenshot_path", observation_context)
        if not isinstance(screenshots[expected_index], str) or screenshots[expected_index] != serialized_path:
            raise AnalysisError(f"{observation_context} disagrees with the legacy screenshots ledger")
        scheduled = _number(observation, "scheduled_capture_seconds", observation_context)
        if scheduled <= previous_time or scheduled < 0 or scheduled > duration + 0.001:
            raise AnalysisError(f"{observation_context} has an invalid scheduled capture time")
        previous_time = scheduled
        translation = _finite_vector(
            _field(observation, "player_camera_translation_metres", observation_context),
            3,
            f"{observation_context}.player_camera_translation_metres",
        )
        rotation = _finite_vector(
            _field(observation, "player_camera_rotation_xyzw", observation_context),
            4,
            f"{observation_context}.player_camera_rotation_xyzw",
        )
        norm_squared = sum(component * component for component in rotation)
        if not 0.999 <= norm_squared <= 1.001:
            raise AnalysisError(f"{observation_context} camera quaternion is not normalized")
        image_path = _observation_path(run_dir, serialized_path, index, observation_context)
        if image_path.name in referenced_names:
            raise AnalysisError(f"{observation_context} duplicates a screenshot file")
        referenced_names.add(image_path.name)
        _, snapshot = _safe_read(image_path, MAX_IMAGE_BYTES, f"{context} image {index}")
        measured_width, measured_height, pixel_count, wall_pixels, occupancy = _measure_png(
            snapshot, width, height, f"{context} image {index}"
        )
        evidence.append(
            ImageEvidence(
                capture_index=index,
                screenshot_name=image_path.name,
                scheduled_capture_seconds=scheduled,
                translation=(translation[0], translation[1], translation[2]),
                rotation=(rotation[0], rotation[1], rotation[2], rotation[3]),
                width=measured_width,
                height=measured_height,
                viewport_pixels=pixel_count,
                wall_pixels=wall_pixels,
                occupancy=occupancy,
                snapshot=snapshot,
            )
        )

    direct_png_names = {path.name for path in run_dir.iterdir() if path.suffix.lower() == ".png"}
    if direct_png_names != referenced_names:
        missing = sorted(referenced_names - direct_png_names)
        unbound = sorted(direct_png_names - referenced_names)
        raise AnalysisError(
            f"{context} PNG set disagrees with the report ledger; missing={missing}, unbound={unbound}"
        )
    return evidence


def _load_run(raw_path: str | os.PathLike[str], spec: ArmSpec) -> RunEvidence:
    run_dir = _validate_run_dir(raw_path, spec.key)
    report, report_snapshot = _load_report(run_dir, spec)
    identity, viewport, streaming, plan_hash, variant_index, schema_generation = (
        _validate_identity(report, spec)
    )
    images = _load_images(report, run_dir, spec, viewport)
    return RunEvidence(
        spec=spec,
        run_dir=run_dir,
        report=report,
        report_snapshot=report_snapshot,
        images=images,
        identity=identity,
        viewport=viewport,
        streaming=streaming,
        route_plan_hash=plan_hash,
        route_variant_index=variant_index,
        report_schema_generation=schema_generation,
    )


def _quaternion_delta_degrees(
    first: Sequence[float], second: Sequence[float]
) -> float:
    # q and -q represent the same orientation.
    dot = abs(sum(left * right for left, right in zip(first, second)))
    dot = min(1.0, max(-1.0, dot))
    return math.degrees(2.0 * math.acos(dot))


def _position_delta(first: Sequence[float], second: Sequence[float]) -> float:
    return math.sqrt(sum((left - right) ** 2 for left, right in zip(first, second)))


def _validate_common_identity(runs: Sequence[RunEvidence]) -> dict[str, Any]:
    if len({run.run_dir for run in runs}) != len(runs):
        raise AnalysisError("all four run directories must be distinct")
    schema_generations = {run.report_schema_generation for run in runs}
    if len(schema_generations) != 1:
        raise AnalysisError("diagnostic schema generations differ across the four arms")
    schema_generation = next(iter(schema_generations))
    fields = (
        "package_version",
        "build_profile",
        "git_sha",
        "git_dirty",
        "source_fingerprint",
        "executable_hash",
        "toolchain",
        "hardware",
        "world_seed",
        "scenery_quality",
        "terrain_grammar",
    )
    common: dict[str, Any] = {}
    for field_name in fields:
        values = [run.identity[field_name] for run in runs]
        if any(value != values[0] for value in values[1:]):
            raise AnalysisError(f"run_identity.{field_name} differs across the four arms")
        common[field_name] = values[0]

    viewport_fields = [
        "logical_width",
        "logical_height",
        "physical_width",
        "physical_height",
        "scale_factor",
        "dpi_percent",
    ]
    if schema_generation == "2.6.0":
        viewport_fields.append("base_scale_factor")
    for field_name in viewport_fields:
        values = [run.viewport[field_name] for run in runs]
        if any(value != values[0] for value in values[1:]):
            raise AnalysisError(f"viewport.{field_name} differs across the four arms")

    schedules = [tuple(image.scheduled_capture_seconds for image in run.images) for run in runs]
    indices = [tuple(image.capture_index for image in run.images) for run in runs]
    names = [tuple(image.screenshot_name for image in run.images) for run in runs]
    if any(value != indices[0] for value in indices[1:]):
        raise AnalysisError("capture-index sets differ across the four arms")
    if any(value != names[0] for value in names[1:]):
        raise AnalysisError("screenshot phase/name sets differ across the four arms")
    for arm_schedule in schedules[1:]:
        if len(arm_schedule) != len(schedules[0]) or any(
            abs(left - right) > MAX_PAIRED_CAPTURE_TIME_DELTA_SECONDS
            for left, right in zip(schedules[0], arm_schedule)
        ):
            raise AnalysisError("scheduled capture times differ across the four arms")
    return common


def _topology_signature(run: RunEvidence) -> tuple[Any, ...]:
    streaming = run.streaming
    names = (
        "resident_entities",
        "resident_vertices",
        "resident_indices",
        "ring_vertices",
        "ring_indices",
        "resident_mesh_bytes",
        "scheduler_resident_entities",
        "scheduler_resident_vertices",
        "scheduler_resident_indices",
        "scheduler_ring_vertices",
        "scheduler_ring_indices",
        "scheduler_resident_mesh_bytes",
    )
    return tuple(
        tuple(streaming[name]) if isinstance(streaming[name], list) else streaming[name]
        for name in names
    )


def _pair_frames(
    profile: str, baseline: RunEvidence, candidate: RunEvidence
) -> tuple[list[dict[str, Any]], bool, bool]:
    if baseline.route_plan_hash != candidate.route_plan_hash:
        raise AnalysisError(f"{profile} camera_route_plan_hash differs between arms")
    if baseline.route_variant_index != candidate.route_variant_index:
        raise AnalysisError(f"{profile} camera route variant differs between arms")
    if _topology_signature(baseline) != _topology_signature(candidate):
        raise AnalysisError(f"{profile} terrain topology differs between point and candidate arms")
    if len(baseline.images) != len(candidate.images):
        raise AnalysisError(f"{profile} capture counts differ between arms")

    frames: list[dict[str, Any]] = []
    every_half_pass = True
    every_absolute_pass = True
    for point, cardinal in zip(baseline.images, candidate.images):
        if point.capture_index != cardinal.capture_index or point.screenshot_name != cardinal.screenshot_name:
            raise AnalysisError(f"{profile} frame ledgers are not index/name matched")
        if point.viewport_pixels != cardinal.viewport_pixels:
            raise AnalysisError(f"{profile} frame {point.capture_index} viewport sizes differ")
        time_delta = abs(point.scheduled_capture_seconds - cardinal.scheduled_capture_seconds)
        position_delta = _position_delta(point.translation, cardinal.translation)
        rotation_delta = _quaternion_delta_degrees(point.rotation, cardinal.rotation)
        if time_delta > MAX_PAIRED_CAPTURE_TIME_DELTA_SECONDS:
            raise AnalysisError(f"{profile} frame {point.capture_index} capture time is not matched")
        if position_delta > MAX_PAIRED_POSITION_DELTA_METRES:
            raise AnalysisError(
                f"{profile} frame {point.capture_index} camera position delta {position_delta:.6f} m exceeds the tight tolerance"
            )
        if rotation_delta > MAX_PAIRED_ROTATION_DELTA_DEGREES:
            raise AnalysisError(
                f"{profile} frame {point.capture_index} camera rotation delta {rotation_delta:.6f} deg exceeds the tight tolerance"
            )

        if point.wall_pixels == 0:
            half_pass = cardinal.wall_pixels == 0
            candidate_to_baseline = None
            reduction_fraction = None
        else:
            candidate_to_baseline = cardinal.wall_pixels / point.wall_pixels
            reduction_fraction = (point.wall_pixels - cardinal.wall_pixels) / point.wall_pixels
            half_pass = candidate_to_baseline <= MAX_CANDIDATE_TO_BASELINE
        absolute_pass = cardinal.occupancy <= MAX_CANDIDATE_VIEWPORT_OCCUPANCY
        every_half_pass &= half_pass
        every_absolute_pass &= absolute_pass
        frames.append(
            {
                "capture_index": point.capture_index,
                "screenshot_name": point.screenshot_name,
                "scheduled_capture_seconds": point.scheduled_capture_seconds,
                "pose_delta": {
                    "position_metres": position_delta,
                    "rotation_degrees": rotation_delta,
                    "capture_time_seconds": time_delta,
                },
                "viewport_pixels": point.viewport_pixels,
                "point": {
                    "wall_pixels": point.wall_pixels,
                    "occupancy": point.occupancy,
                    "image_sha256": point.snapshot.sha256,
                    "camera_pose": {
                        "translation_metres": list(point.translation),
                        "rotation_xyzw": list(point.rotation),
                    },
                },
                "cardinal_trimmed": {
                    "wall_pixels": cardinal.wall_pixels,
                    "occupancy": cardinal.occupancy,
                    "image_sha256": cardinal.snapshot.sha256,
                    "camera_pose": {
                        "translation_metres": list(cardinal.translation),
                        "rotation_xyzw": list(cardinal.rotation),
                    },
                },
                "candidate_to_baseline_ratio": candidate_to_baseline,
                "wall_pixel_reduction_fraction": reduction_fraction,
                "zero_baseline_rule_applied": point.wall_pixels == 0,
                "stop_test_1_half_baseline_pass": half_pass,
                "stop_test_2_absolute_five_percent_pass": absolute_pass,
            }
        )
    return frames, every_half_pass, every_absolute_pass


def _run_summary(run: RunEvidence) -> dict[str, Any]:
    streaming = run.streaming
    return {
        "run_directory": str(run.run_dir),
        "report_path": str(run.report_snapshot.path),
        "report_sha256": run.report_snapshot.sha256,
        "report_size_bytes": run.report_snapshot.size_bytes,
        "qa_report_schema_version": run.report["qa_report_schema_version"],
        "evidence_disposition": run.report["evidence_disposition"],
        "world_profile": run.spec.world_profile,
        "l0_height_mode": run.spec.mode,
        "surface_material_mode": streaming["surface_material_mode"],
        "camera_route_plan_hash": run.route_plan_hash,
        "camera_route_variant_index": run.route_variant_index,
        "capture_count": len(run.images),
        "live_sample_cache_bytes": streaming["live_sample_cache_bytes"],
        "peak_live_sample_cache_bytes": streaming["peak_live_sample_cache_bytes"],
        "topology": {
            "entities": streaming["resident_entities"],
            "vertices": streaming["resident_vertices"],
            "indices": streaming["resident_indices"],
            "ring_vertices": streaming["ring_vertices"],
            "ring_indices": streaming["ring_indices"],
            "mesh_bytes": streaming["resident_mesh_bytes"],
        },
    }


def _pair_binding(baseline: RunEvidence, candidate: RunEvidence) -> dict[str, Any]:
    """Hashes and route identity that cryptographically bind one profile pair."""

    return {
        "point_report_sha256": baseline.report_snapshot.sha256,
        "cardinal_trimmed_report_sha256": candidate.report_snapshot.sha256,
        "executable_hash": baseline.identity["executable_hash"],
        "source_fingerprint": baseline.identity["source_fingerprint"],
        "git_sha": baseline.identity["git_sha"],
        "build_profile": baseline.identity["build_profile"],
        "camera_route_plan_hash": baseline.route_plan_hash,
        "camera_route_variant_index": baseline.route_variant_index,
    }


def _verify_all_inputs_unchanged(runs: Iterable[RunEvidence]) -> None:
    for run in runs:
        _verify_snapshot_unchanged(run.report_snapshot, MAX_REPORT_BYTES, f"{run.spec.key} report")
        for image in run.images:
            _verify_snapshot_unchanged(
                image.snapshot,
                MAX_IMAGE_BYTES,
                f"{run.spec.key} image {image.capture_index}",
            )


def analyze(
    natural_point: str | os.PathLike[str],
    natural_cardinal: str | os.PathLike[str],
    astral_point: str | os.PathLike[str],
    astral_cardinal: str | os.PathLike[str],
) -> dict[str, Any]:
    """Validate and analyze four explicit run directories.

    Raises ``AnalysisError`` when evidence is malformed, mismatched, unsafe, or
    fails a structural stop test.  A valid mask comparison returns a ledger;
    its decision is ``reject`` when either numeric occupancy stop test fails.
    """

    paths = (natural_point, natural_cardinal, astral_point, astral_cardinal)
    runs = [_load_run(path, spec) for path, spec in zip(paths, ARM_SPECS)]
    common_identity = _validate_common_identity(runs)
    natural_frames, natural_half, natural_absolute = _pair_frames(
        "Natural", runs[0], runs[1]
    )
    astral_frames, astral_half, astral_absolute = _pair_frames(
        "Astral", runs[2], runs[3]
    )
    stop_1 = natural_half and astral_half
    stop_2 = natural_absolute and astral_absolute
    automated_pass = stop_1 and stop_2

    _verify_all_inputs_unchanged(runs)
    return {
        "analysis_schema_version": ANALYZER_SCHEMA_VERSION,
        "evidence_disposition": EVIDENCE_DISPOSITION,
        "generated_utc": dt.datetime.now(dt.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z"),
        "mask_contract": {
            "predicate": "(R > 200) & (G < 10) & (B < 30)",
            "connectivity": "largest-8-connected-non-background-component",
            "denominator": "physical-viewport-width-times-height",
        },
        "input_contract": {
            "fixed_physical_viewport": {
                "width": EXPECTED_VIEWPORT[0],
                "height": EXPECTED_VIEWPORT[1],
            },
            "canonical_screenshot_path": (
                "<run-parent>\\<run-directory>\\<shot-name>"
            ),
            "settled_point_cache_bytes": 228_822,
            "settled_cardinal_cache_bytes": 263_142,
        },
        "pairing_tolerances": {
            "position_metres": MAX_PAIRED_POSITION_DELTA_METRES,
            "rotation_degrees": MAX_PAIRED_ROTATION_DELTA_DEGREES,
            "scheduled_capture_seconds": MAX_PAIRED_CAPTURE_TIME_DELTA_SECONDS,
        },
        "common_identity": common_identity,
        "runs": {run.spec.key: _run_summary(run) for run in runs},
        "profiles": {
            "Natural": {
                "pair_identity": _pair_binding(runs[0], runs[1]),
                "frames": natural_frames,
            },
            "Astral": {
                "pair_identity": _pair_binding(runs[2], runs[3]),
                "frames": astral_frames,
            },
        },
        "stop_tests": {
            "1_every_candidate_frame_at_most_half_baseline": stop_1,
            "2_every_candidate_frame_at_most_five_percent_viewport": stop_2,
            "3_human_visual_inspection": "required-not-evaluated-by-this-tool",
            "4_identity_scheduler_and_observation_health": True,
            "5_candidate_cache_at_most_263142_bytes": True,
            "6_matched_topology": True,
        },
        "automated_decision": (
            "pass-pending-mandatory-human-inspection" if automated_pass else "reject"
        ),
        "canonical_publishable": False,
    }


def _validate_output_path(
    raw_path: str | os.PathLike[str], input_dirs: Sequence[Path]
) -> Path:
    lexical = Path(os.path.abspath(raw_path))
    if lexical.suffix.lower() != ".json":
        raise AnalysisError("output path must use the .json suffix")
    if any(part.casefold() in PROTECTED_OUTPUT_COMPONENTS for part in lexical.parts):
        raise AnalysisError("output path may not be inside saves, qa_runs, or agent_runs")
    parent = lexical.parent
    _reject_symlink_components(parent, "output parent")
    try:
        resolved_parent = parent.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise AnalysisError(f"output parent does not resolve safely: {error}") from error
    # Windows accepts aliases such as ``qa_runs `` and ``QA_RUNS. `` for an
    # existing ``qa_runs`` directory.  Checking only the lexical components
    # would therefore allow analysis output into protected evidence.  Require
    # the parent spelling to survive canonicalization and enforce the protected
    # component rule again on the path the OS will actually use.
    if _path_comparison_key(parent) != _path_comparison_key(resolved_parent):
        raise AnalysisError("output parent resolves through an alias or symlink")
    if any(
        part.casefold() in PROTECTED_OUTPUT_COMPONENTS
        for part in resolved_parent.parts
    ):
        raise AnalysisError("output path may not be inside saves, qa_runs, or agent_runs")
    if not resolved_parent.is_dir():
        raise AnalysisError("output parent is not a directory")
    output = resolved_parent / lexical.name
    if output.exists() or output.is_symlink():
        raise AnalysisError("output path already exists; evidence is never overwritten")
    comparable_output = Path(_path_comparison_key(output))
    for run_dir in input_dirs:
        try:
            comparable_output.relative_to(Path(_path_comparison_key(run_dir)))
        except ValueError:
            continue
        raise AnalysisError("output path may not be inside an input run directory")
    return output


def write_json_create_new_atomic(
    raw_path: str | os.PathLike[str], ledger: dict[str, Any], input_dirs: Sequence[Path]
) -> Path:
    """Atomically publish a complete JSON file without overwrite semantics."""

    output = _validate_output_path(raw_path, input_dirs)
    payload = (json.dumps(ledger, indent=2, sort_keys=True) + "\n").encode("utf-8")
    if len(payload) > 16 * 1024 * 1024:
        raise AnalysisError("analysis JSON exceeds the 16 MiB output cap")
    temporary = output.parent / f".{output.name}.{secrets.token_hex(8)}.tmp"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0)
    descriptor: int | None = None
    try:
        descriptor = os.open(temporary, flags, 0o600)
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise AnalysisError("atomic output write made no progress")
            view = view[written:]
        os.fsync(descriptor)
        os.close(descriptor)
        descriptor = None
        # Hard-link publication is atomic and fails if output appeared after
        # validation.  Unlike replace/rename, it never overwrites evidence.
        os.link(temporary, output)
        try:
            directory_fd = os.open(output.parent, os.O_RDONLY)
        except OSError:
            directory_fd = None
        if directory_fd is not None:
            try:
                os.fsync(directory_fd)
            except OSError:
                pass
            finally:
                os.close(directory_fd)
    except FileExistsError as error:
        raise AnalysisError("output path appeared concurrently; refusing overwrite") from error
    except OSError as error:
        raise AnalysisError(f"atomic create-new output failed: {error}") from error
    finally:
        if descriptor is not None:
            os.close(descriptor)
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
        except OSError:
            pass
    return output


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Validate and measure four matched LodProvenanceV1 QA runs. "
            "Exit 0 means automated pass pending human inspection; exit 1 "
            "means the occupancy stop test rejected the candidate; exit 2 "
            "means the evidence was invalid."
        )
    )
    parser.add_argument("--natural-point", required=True, help="Natural Point16V1 run directory")
    parser.add_argument(
        "--natural-cardinal", required=True, help="Natural CardinalTrimmed8V1 run directory"
    )
    parser.add_argument("--astral-point", required=True, help="Astral Point16V1 run directory")
    parser.add_argument(
        "--astral-cardinal", required=True, help="Astral CardinalTrimmed8V1 run directory"
    )
    parser.add_argument(
        "--output",
        help="optional new .json path; parent must exist and existing files are never overwritten",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        ledger = analyze(
            arguments.natural_point,
            arguments.natural_cardinal,
            arguments.astral_point,
            arguments.astral_cardinal,
        )
        if arguments.output:
            input_dirs = [
                Path(ledger["runs"][spec.key]["run_directory"]) for spec in ARM_SPECS
            ]
            write_json_create_new_atomic(arguments.output, ledger, input_dirs)
        print(json.dumps(ledger, indent=2, sort_keys=True))
        return 0 if ledger["automated_decision"].startswith("pass-") else 1
    except AnalysisError as error:
        print(f"L0 provenance analysis invalid: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
