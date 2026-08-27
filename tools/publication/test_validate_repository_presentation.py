#!/usr/bin/env python3
"""Focused guards for the public repository presentation validator."""

from __future__ import annotations

from contextlib import contextmanager
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import validate_repository_presentation as presentation


def valid_pdf_bytes() -> bytes:
    return b"%PDF-1.7\n" + (b"0" * 50_000) + b"\n%%EOF\n"


def valid_pdf_with_stream(payload: bytes, outside_stream: bytes = b"") -> bytes:
    padding = b"0" * max(0, 50_000 - len(payload))
    return (
        b"%PDF-1.7\n1 0 obj\n<< /Length 50000 >>\nstream\n"
        + payload
        + padding
        + b"\nendstream\nendobj\n"
        + outside_stream
        + b"\n%%EOF\n"
    )


@contextmanager
def patched_publication_root(root: Path):
    media_root = root / "docs" / "media"
    release_root = root / "docs" / "releases"
    canonical_pdf = (
        release_root
        / "technical-preview"
        / "voxel-native-codex-engineering-atlas.pdf"
    )
    with mock.patch.multiple(
        presentation,
        ROOT=root,
        MEDIA_ROOT=media_root,
        RELEASE_ROOT=release_root,
        CANONICAL_RELEASE_PDF=canonical_pdf,
    ):
        yield media_root, release_root, canonical_pdf


class PublicTextGuardTests(unittest.TestCase):
    def test_valid_text_and_web_urls_are_not_workstation_paths(self) -> None:
        text = "Research: https://example.test/home/alice/paper and Cauchy: z / home."
        self.assertIsNone(presentation.invalid_scalar(text))
        self.assertFalse(presentation.contains_workstation_path(text))

    def test_windows_drive_and_unc_paths_are_rejected(self) -> None:
        self.assertTrue(
            presentation.contains_workstation_path(r"C:\Users\alice\artifact.pdf")
        )
        self.assertTrue(
            presentation.contains_workstation_path(r"\\server\share\artifact.pdf")
        )

    def test_common_absolute_posix_workstation_paths_are_rejected(self) -> None:
        for path in (
            "/Users/alice/artifact.pdf",
            "/home/alice/artifact.pdf",
            "/workspace/build/artifact.pdf",
            "/tmp/artifact.pdf",
        ):
            with self.subTest(path=path):
                self.assertTrue(presentation.contains_workstation_path(path))

    def test_controls_replacement_characters_and_noncharacters_are_rejected(self) -> None:
        expected = {
            "\x00": "U+0000 control",
            "\x7f": "U+007F control",
            "\ufffd": "U+FFFD replacement character",
            "\ufdd0": "U+FDD0 noncharacter",
            "\U0010ffff": "U+10FFFF noncharacter",
        }
        for character, message in expected.items():
            with self.subTest(codepoint=ord(character)):
                self.assertEqual(presentation.invalid_scalar(character), message)

    def test_tab_line_feed_and_carriage_return_remain_valid(self) -> None:
        self.assertIsNone(presentation.invalid_scalar("alpha\tbeta\ngamma\r\n"))

    def test_link_discovery_ignores_web_and_fragment_targets(self) -> None:
        text = "[local](docs/atlas.md) [web](https://example.test/a) [same](#scope)"
        self.assertEqual(
            presentation.local_targets(Path("README.md"), text),
            ["docs/atlas.md"],
        )


class ReleasePdfGuardTests(unittest.TestCase):
    def test_missing_canonical_release_pdf_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with patched_publication_root(root):
                with self.assertRaisesRegex(
                    presentation.PresentationError,
                    "missing canonical release PDF",
                ):
                    presentation.validate_release_pdfs()

    def test_exact_canonical_release_pdf_is_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with patched_publication_root(root) as (_, _, canonical_pdf):
                canonical_pdf.parent.mkdir(parents=True)
                canonical_pdf.write_bytes(valid_pdf_bytes())
                self.assertEqual(presentation.validate_release_pdfs(), 1)

    def test_extra_release_pdf_is_rejected_case_insensitively(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with patched_publication_root(root) as (_, release_root, canonical_pdf):
                canonical_pdf.parent.mkdir(parents=True)
                canonical_pdf.write_bytes(valid_pdf_bytes())
                (release_root / "legacy.PDF").write_bytes(valid_pdf_bytes())
                with self.assertRaisesRegex(
                    presentation.PresentationError,
                    "unexpected release PDF",
                ):
                    presentation.validate_release_pdfs()

    def test_invalid_canonical_release_pdf_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with patched_publication_root(root) as (_, _, canonical_pdf):
                canonical_pdf.parent.mkdir(parents=True)
                canonical_pdf.write_bytes(b"%PDF-1.7\n%%EOF\n")
                with self.assertRaisesRegex(
                    presentation.PresentationError,
                    "invalid or unexpectedly small release PDF",
                ):
                    presentation.validate_release_pdfs()

    def test_binary_stream_path_shape_does_not_create_a_false_positive(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with patched_publication_root(root) as (_, _, canonical_pdf):
                canonical_pdf.parent.mkdir(parents=True)
                canonical_pdf.write_bytes(
                    valid_pdf_with_stream(b"compressed-shape C:\\Users\\noise")
                )
                self.assertEqual(presentation.validate_release_pdfs(), 1)

    def test_workstation_path_outside_stream_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with patched_publication_root(root) as (_, _, canonical_pdf):
                canonical_pdf.parent.mkdir(parents=True)
                canonical_pdf.write_bytes(
                    valid_pdf_with_stream(
                        b"opaque payload",
                        b"/Author (C:\\Users\\alice\\private.txt)",
                    )
                )
                with self.assertRaisesRegex(
                    presentation.PresentationError,
                    "absolute workstation path leaked",
                ):
                    presentation.validate_release_pdfs()

    def test_unterminated_stream_is_rejected(self) -> None:
        data = b"%PDF-1.7\n1 0 obj\n<< /Length 50000 >>\nstream\n" + b"0" * 50_000
        with self.assertRaisesRegex(
            presentation.PresentationError,
            "unterminated stream object",
        ):
            presentation.pdf_syntax_without_stream_payloads(data)


class SvgGuardTests(unittest.TestCase):
    def test_minimal_passive_accessible_svg_is_accepted(self) -> None:
        source = (
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">'
            '<title id="title">Title</title><desc id="desc">Description</desc>'
            '<rect id="panel" width="10" height="10" fill="#000000"/></svg>'
        )
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with patched_publication_root(root) as (media_root, _, _):
                media_root.mkdir(parents=True)
                svg = media_root / "valid.svg"
                svg.write_text(source, encoding="utf-8")
                presentation.validate_svg(svg)

    def test_svg_accessibility_and_active_content_guards_fail_closed(self) -> None:
        cases = {
            "missing-viewbox": (
                '<svg xmlns="http://www.w3.org/2000/svg">'
                '<title>Title</title><desc>Description</desc></svg>',
                "missing viewBox",
            ),
            "missing-description": (
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">'
                '<title>Title</title></svg>',
                "needs title and desc",
            ),
            "script": (
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">'
                '<title>Title</title><desc>Description</desc><script/></svg>',
                "active SVG element",
            ),
            "external-resource": (
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">'
                '<title>Title</title><desc>Description</desc>'
                '<a href="https://example.test/asset.svg"/></svg>',
                "external SVG resource",
            ),
            "duplicate-id": (
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">'
                '<title id="same">Title</title><desc id="same">Description</desc>'
                "</svg>",
                "duplicate XML id",
            ),
            "entity": (
                '<!DOCTYPE svg [<!ENTITY unsafe "value">]>'
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">'
                '<title>Title</title><desc>Description</desc></svg>',
                "DTD/entity declaration",
            ),
        }
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            with patched_publication_root(root) as (media_root, _, _):
                media_root.mkdir(parents=True)
                svg = media_root / "fixture.svg"
                for name, (source, expected) in cases.items():
                    with self.subTest(name=name):
                        svg.write_text(source, encoding="utf-8")
                        with self.assertRaisesRegex(
                            presentation.PresentationError,
                            expected,
                        ):
                            presentation.validate_svg(svg)


class MarkdownLinkGuardTests(unittest.TestCase):
    def test_repository_escape_and_absolute_local_link_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            markdown = root / "docs" / "guide.md"
            with patched_publication_root(root):
                for target in ("../../outside.md", "C:/workspace/private.md"):
                    with self.subTest(target=target):
                        with self.assertRaises(presentation.PresentationError):
                            presentation.resolve_public_target(markdown, target)

    def test_missing_link_and_editorial_marker_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            markdown = root / "README.md"
            with patched_publication_root(root), mock.patch.object(
                presentation,
                "PUBLIC_MARKDOWN",
                (markdown,),
            ):
                cases = {
                    "missing-link": (
                        "[missing](docs/missing.md)",
                        "broken local link",
                    ),
                    "editorial-marker": (
                        "LIVE-QA-GALLERY: replace later",
                        "editorial marker",
                    ),
                }
                for name, (source, expected) in cases.items():
                    with self.subTest(name=name):
                        markdown.write_text(source, encoding="utf-8")
                        with self.assertRaisesRegex(
                            presentation.PresentationError,
                            expected,
                        ):
                            presentation.validate_markdown()


if __name__ == "__main__":
    unittest.main(verbosity=2)
