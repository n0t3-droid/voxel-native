#!/usr/bin/env python3
"""Fail closed when the public repository surface contains broken artifacts.

The check intentionally uses only Python's standard library so it can run in CI
without broadening the dependency boundary. It validates identity and structure;
it does not claim that an SVG or PDF is visually acceptable.
"""

from __future__ import annotations

import re
import sys
import urllib.parse
import xml.etree.ElementTree as ET
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PUBLIC_MARKDOWN = tuple(
    path
    for path in (
        *sorted(ROOT.glob("*.md")),
        *sorted((ROOT / "docs").rglob("*.md")),
    )
    if path.name != "AI_HANDOFF.md"
)
MEDIA_ROOT = ROOT / "docs" / "media"
RELEASE_ROOT = ROOT / "docs" / "releases"
CANONICAL_RELEASE_PDF = (
    RELEASE_ROOT
    / "technical-preview"
    / "voxel-native-codex-engineering-atlas.pdf"
)
MAX_MARKDOWN_BYTES = 8 * 1024 * 1024
MAX_SVG_BYTES = 8 * 1024 * 1024
MAX_PDF_BYTES = 64 * 1024 * 1024
MAX_SVG_ELEMENTS = 20_000

MARKDOWN_LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
HTML_LINK = re.compile(r"\b(?:href|src)=[\"']([^\"']+)[\"']", re.IGNORECASE)
ABSOLUTE_LOCAL_LINK = re.compile(r"(?i)^(?:[a-z]:[\\/]|\\\\)")
WEB_URL = re.compile(r"(?i)https?://[^\s<>\"']+")
WINDOWS_ABSOLUTE_PATH = re.compile(r"(?i)(?<![a-z0-9])[a-z]:[\\/]")
WINDOWS_UNC_PATH = re.compile(r"\\\\(?:\?\\UNC\\|[^\\\s]+\\)[^\s]+", re.IGNORECASE)
POSIX_WORKSTATION_PATH = re.compile(
    r"(?i)(?<![a-z0-9:])/(?:users|home|workspace|tmp|var|opt)(?:/|\\)"
)
PDF_STREAM_START = re.compile(rb"(?:\r\n|\r|\n)stream(?:\r\n|\r|\n)")
PDF_STREAM_END_OBJECT = re.compile(
    rb"endstream[\x00\x09\x0A\x0C\x0D\x20]+endobj"
)
EDITORIAL_MARKERS = (
    "LIVE-QA-GALLERY:",
    "LIVE-QA-PROMOTION-SLOTS",
    "MEASURED-A-B-GRAPH",
    "replace this comment",
    "insert one measured",
)


class PresentationError(RuntimeError):
    """A public presentation invariant failed."""


def read_bounded(path: Path, maximum: int, label: str) -> bytes:
    try:
        size = path.stat().st_size
    except OSError as error:
        raise PresentationError(f"cannot stat {label} {path.relative_to(ROOT)}: {error}") from error
    if size > maximum:
        raise PresentationError(
            f"{label} exceeds {maximum} bytes: {path.relative_to(ROOT)} ({size})"
        )
    try:
        data = path.read_bytes()
    except OSError as error:
        raise PresentationError(f"cannot read {label} {path.relative_to(ROOT)}: {error}") from error
    if len(data) != size:
        raise PresentationError(f"{label} changed while read: {path.relative_to(ROOT)}")
    return data


def decode_utf8(data: bytes, path: Path, label: str) -> str:
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise PresentationError(
            f"{label} is not UTF-8: {path.relative_to(ROOT)}: {error}"
        ) from error


def invalid_scalar(text: str) -> str | None:
    for character in text:
        value = ord(character)
        if value < 0x20 and character not in "\t\n\r":
            return f"U+{value:04X} control"
        if value == 0x7F:
            return "U+007F control"
        if 0xD800 <= value <= 0xDFFF:
            return f"U+{value:04X} surrogate"
        if value == 0xFFFD:
            return "U+FFFD replacement character"
        if 0xFDD0 <= value <= 0xFDEF or value & 0xFFFF in (0xFFFE, 0xFFFF):
            return f"U+{value:04X} noncharacter"
    return None


def contains_workstation_path(text: str) -> bool:
    without_urls = WEB_URL.sub("", text)
    return any(
        pattern.search(without_urls)
        for pattern in (
            WINDOWS_ABSOLUTE_PATH,
            WINDOWS_UNC_PATH,
            POSIX_WORKSTATION_PATH,
        )
    )


def pdf_syntax_without_stream_payloads(data: bytes) -> bytes:
    """Return PDF syntax while excluding opaque compressed/binary stream bytes.

    Raw Flate or ASCII85 payloads can coincidentally contain sequences shaped
    like local paths. The presentation check therefore examines only public PDF
    syntax outside streams. The canonical atlas builder's ``--validate-release``
    route remains responsible for strict object-graph traversal and extracted-
    text path rejection after decompression.
    """

    chunks: list[bytes] = []
    cursor = 0
    while True:
        start = PDF_STREAM_START.search(data, cursor)
        if start is None:
            chunks.append(data[cursor:])
            break
        chunks.append(data[cursor : start.end()])
        end = PDF_STREAM_END_OBJECT.search(data, start.end())
        if end is None:
            raise PresentationError("release PDF contains an unterminated stream object")
        chunks.append(data[end.start() : end.end()])
        cursor = end.end()
    return b"".join(chunks)


def guard_public_text(text: str, path: Path, label: str) -> None:
    invalid = invalid_scalar(text)
    if invalid is not None:
        raise PresentationError(f"{invalid} leaked into {label} {path.relative_to(ROOT)}")
    if contains_workstation_path(text):
        raise PresentationError(f"absolute workstation path leaked into {label} {path.relative_to(ROOT)}")


def local_targets(markdown: Path, text: str) -> list[str]:
    raw_targets = [*MARKDOWN_LINK.findall(text), *HTML_LINK.findall(text)]
    targets: list[str] = []
    for raw in raw_targets:
        target = raw.strip()
        if target.startswith("<") and target.endswith(">"):
            target = target[1:-1]
        # Markdown permits an optional quoted title after a whitespace boundary.
        target = re.split(r"\s+[\"']", target, maxsplit=1)[0]
        lower = target.lower()
        if lower.startswith(("https://", "http://", "mailto:", "data:")):
            continue
        if not target or target.startswith("#"):
            continue
        targets.append(target)
    return targets


def resolve_public_target(markdown: Path, target: str) -> Path:
    path_text = urllib.parse.unquote(target.split("#", 1)[0].split("?", 1)[0])
    if not path_text:
        return markdown
    if path_text.startswith(("/", "\\")) or ABSOLUTE_LOCAL_LINK.search(path_text):
        raise PresentationError(f"absolute local link in {markdown.relative_to(ROOT)}: {target}")
    candidate = (markdown.parent / Path(path_text.replace("/", str(Path('/'))))).resolve()
    try:
        candidate.relative_to(ROOT)
    except ValueError as error:
        raise PresentationError(
            f"link escapes repository in {markdown.relative_to(ROOT)}: {target}"
        ) from error
    return candidate


def validate_markdown() -> int:
    checked = 0
    for markdown in PUBLIC_MARKDOWN:
        if not markdown.is_file():
            raise PresentationError(f"missing public document: {markdown.relative_to(ROOT)}")
        text = decode_utf8(
            read_bounded(markdown, MAX_MARKDOWN_BYTES, "Markdown"),
            markdown,
            "Markdown",
        )
        guard_public_text(text, markdown, "Markdown")
        for marker in EDITORIAL_MARKERS:
            if marker.casefold() in text.casefold():
                raise PresentationError(
                    f"editorial marker {marker!r} remains in {markdown.relative_to(ROOT)}"
                )
        for target in local_targets(markdown, text):
            resolved = resolve_public_target(markdown, target)
            if not resolved.exists():
                raise PresentationError(
                    f"broken local link in {markdown.relative_to(ROOT)}: {target}"
                )
            checked += 1
    return checked


def local_name(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def validate_svg(path: Path) -> None:
    data = read_bounded(path, MAX_SVG_BYTES, "SVG")
    text = decode_utf8(data, path, "SVG")
    guard_public_text(text, path, "SVG")
    lowered = text.casefold()
    if "<!doctype" in lowered or "<!entity" in lowered:
        raise PresentationError(f"DTD/entity declaration is forbidden in SVG: {path.relative_to(ROOT)}")
    try:
        root = ET.fromstring(data)
    except ET.ParseError as error:
        raise PresentationError(f"invalid SVG {path.relative_to(ROOT)}: {error}") from error
    if local_name(root.tag) != "svg":
        raise PresentationError(f"non-SVG XML root in {path.relative_to(ROOT)}")
    if not root.attrib.get("viewBox", "").strip():
        raise PresentationError(f"missing viewBox in {path.relative_to(ROOT)}")

    elements = list(root.iter())
    if len(elements) > MAX_SVG_ELEMENTS:
        raise PresentationError(
            f"SVG exceeds {MAX_SVG_ELEMENTS} elements: {path.relative_to(ROOT)}"
        )
    names = [local_name(element.tag).casefold() for element in elements]
    if "title" not in names or "desc" not in names:
        raise PresentationError(f"SVG needs title and desc: {path.relative_to(ROOT)}")
    forbidden = sorted(set(names).intersection({"script", "foreignobject"}))
    if forbidden:
        raise PresentationError(
            f"active SVG element is forbidden in {path.relative_to(ROOT)}: {forbidden}"
        )

    ids = [element.attrib["id"] for element in elements if "id" in element.attrib]
    if len(ids) != len(set(ids)):
        raise PresentationError(f"duplicate XML id in {path.relative_to(ROOT)}")

    for element in elements:
        for attribute, value in element.attrib.items():
            attribute_name = local_name(attribute).casefold()
            if attribute_name.startswith("on"):
                raise PresentationError(
                    f"event handler is forbidden in SVG {path.relative_to(ROOT)}: {attribute_name}"
                )
            if attribute_name != "href":
                continue
            if value and not value.startswith(("#", "data:")):
                raise PresentationError(
                    f"external SVG resource in {path.relative_to(ROOT)}: {value}"
                )


def validate_media() -> int:
    if not MEDIA_ROOT.is_dir():
        raise PresentationError("missing docs/media directory")
    svgs = sorted(MEDIA_ROOT.glob("*.svg"))
    if not svgs:
        raise PresentationError("docs/media contains no authored SVGs")
    for svg in svgs:
        validate_svg(svg)
    return len(svgs)


def validate_release_pdfs() -> int:
    pdfs = sorted(
        path
        for path in RELEASE_ROOT.rglob("*")
        if path.suffix.casefold() == ".pdf"
    )
    canonical_is_regular = (
        CANONICAL_RELEASE_PDF.is_file()
        and not CANONICAL_RELEASE_PDF.is_symlink()
    )
    extras = [pdf for pdf in pdfs if pdf != CANONICAL_RELEASE_PDF]
    if not canonical_is_regular or extras or len(pdfs) != 1:
        details: list[str] = []
        if not canonical_is_regular:
            details.append(
                "missing canonical release PDF "
                f"{CANONICAL_RELEASE_PDF.relative_to(ROOT)}"
            )
        if extras:
            rendered = ", ".join(str(pdf.relative_to(ROOT)) for pdf in extras)
            details.append(f"unexpected release PDF(s): {rendered}")
        if not details:
            details.append(f"expected exactly one release PDF, found {len(pdfs)}")
        raise PresentationError("; ".join(details))

    data = read_bounded(CANONICAL_RELEASE_PDF, MAX_PDF_BYTES, "release PDF")
    if (
        len(data) < 50_000
        or not data.startswith(b"%PDF-")
        or re.search(rb"%%EOF[\x00\x09\x0A\x0C\x0D\x20]*\Z", data) is None
    ):
        raise PresentationError(
            "invalid or unexpectedly small release PDF: "
            f"{CANONICAL_RELEASE_PDF.relative_to(ROOT)}"
        )
    public_syntax = pdf_syntax_without_stream_payloads(data)
    if contains_workstation_path(public_syntax.decode("latin-1", errors="ignore")):
        raise PresentationError(
            "absolute workstation path leaked into release PDF: "
            f"{CANONICAL_RELEASE_PDF.relative_to(ROOT)}"
        )
    return 1


def main() -> int:
    try:
        links = validate_markdown()
        svgs = validate_media()
        pdfs = validate_release_pdfs()
    except PresentationError as error:
        print(f"presentation validation failed: {error}", file=sys.stderr)
        return 1
    print(
        "presentation validation passed: "
        f"{links} local links, {svgs} authored SVGs, {pdfs} release PDFs"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
