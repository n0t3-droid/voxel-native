#!/usr/bin/env python3
"""Pure, no-output safety tests for the source-first atlas builder."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path
from typing import Callable
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
BUILDER_PATH = ROOT / "tools" / "artifacts" / "build_codex_engineering_atlas.py"
SPEC = importlib.util.spec_from_file_location("codex_engineering_atlas_builder", BUILDER_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load atlas builder test target")
ATLAS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ATLAS
SPEC.loader.exec_module(ATLAS)


def snapshot(svg: str, relative: str = "fixture.svg") -> object:
    data = svg.encode("utf-8")
    return ATLAS.InputSnapshot(
        relative=relative,
        path=ROOT / relative,
        data=data,
        text=svg,
        sha256=hashlib.sha256(data).hexdigest(),
    )


def svg_with(content: str) -> str:
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">{content}</svg>'


class SvgFontAndCssSafetyTests(unittest.TestCase):
    def assert_svg_rejected(self, content: str, message: str | None = None) -> None:
        with self.assertRaises(ATLAS.AtlasBuildError) as caught:
            ATLAS.validate_passive_svg(snapshot(svg_with(content)))
        if message is not None:
            self.assertIn(message, str(caught.exception))

    def test_every_declared_repository_svg_uses_canonical_fonts_and_passes(self) -> None:
        for relative in ATLAS.REQUIRED_ASSETS:
            with self.subTest(relative=relative):
                source = ATLAS.read_input_snapshot(ROOT, relative, "SVG asset")
                normalized = ATLAS.validate_passive_svg(source)
                self.assertTrue(normalized.startswith(b"<svg"))

    def test_only_base_helvetica_and_courier_names_are_accepted_as_input(self) -> None:
        for family in sorted(ATLAS.SVG_INPUT_FONT_FAMILIES):
            with self.subTest(family=family):
                normalized = ATLAS.validate_passive_svg(
                    snapshot(svg_with(f'<text font-family="{family}">safe</text>'))
                )
                self.assertIn(family.encode("ascii"), normalized)

        for family in sorted(
            ATLAS.CANONICAL_SVG_FONT_FAMILIES - ATLAS.SVG_INPUT_FONT_FAMILIES
        ):
            for weight in ("normal", "bold"):
                for style in ("normal", "italic", "oblique"):
                    with self.subTest(
                        variant_family=family, weight=weight, style=style
                    ):
                        self.assert_svg_rejected(
                            f"<style>.modifier{{font-weight:{weight};font-style:{style}}}</style>"
                            f'<g font-family="{family}">'
                            '<text class="modifier">unsafe</text></g>',
                            "font family",
                        )

    def test_workstation_font_path_and_unknown_family_are_rejected(self) -> None:
        unsafe_families = (
            ("C:/Windows/Fonts/arial", "font family"),
            ("Inter", "font family"),
            ("Helvetica,Arial", "font family"),
            ("local(Helvetica)", "escaped or environment-dependent value"),
            ("url(#font)", "font family"),
        )
        for family, message in unsafe_families:
            with self.subTest(family=family):
                self.assert_svg_rejected(
                    f'<text font-family="{family}">unsafe</text>',
                    message,
                )

    def test_explicit_canonical_font_properties_and_css_numeric_values_are_allowed(self) -> None:
        style = (
            "<style>"
            ".heading{font-family:Helvetica;font-size:12px;font-style:italic;"
            "font-weight:bold;letter-spacing:1.5px;fill:#fff}"
            ".code{font-family:Courier;font-size:10pt;font-weight:normal;stroke-width:2;"
            "transform:translate(1 2) scale(1.5);fill:none}"
            "</style><text class=\"heading\">safe</text>"
        )
        ATLAS.validate_passive_svg(snapshot(svg_with(style)))

    def test_css_font_paths_sources_and_unknown_families_are_rejected(self) -> None:
        declarations = (
            "font:bold 12px C:/Windows/Fonts/arial",
            "font:bold 12px UnknownFamily",
            "font-family:Arial",
            "font-family:local(Helvetica)",
            "font-family:url(#font)",
        )
        for declaration in declarations:
            with self.subTest(declaration=declaration):
                self.assert_svg_rejected(
                    f"<style>.x{{{declaration}}}</style><text class=\"x\">unsafe</text>"
                )

    def test_css_font_shorthand_and_numeric_weights_are_rejected(self) -> None:
        unsafe = (
            "<style>.x{font:bold 12px Helvetica}</style><text class=\"x\">unsafe</text>",
            "<style>.x{font-family:Helvetica;font-size:12px;font-weight:700}</style>"
            '<text class="x">unsafe</text>',
            '<text font-family="Helvetica" font-weight="700">unsafe</text>',
        )
        for content in unsafe:
            with self.subTest(content=content):
                self.assert_svg_rejected(content)

    def test_invalid_font_is_rejected_before_converter_is_called(self) -> None:
        calls = 0

        def converter(_: object) -> object:
            nonlocal calls
            calls += 1
            raise AssertionError("converter must not be reached")

        with self.assertRaises(ATLAS.AtlasBuildError):
            ATLAS.parse_svg_drawing(
                snapshot(svg_with('<text font-family="Arial">unsafe</text>')),
                converter,
            )
        self.assertEqual(calls, 0)

    def test_css_is_fail_closed_and_numeric_values_are_bounded(self) -> None:
        unsafe_declarations = (
            ("font-size:1e999px", "out-of-range number"),
            ("stroke-width:1e999", "out-of-range number"),
            ("transform:scale(1e999)", "out-of-range number"),
            ("unknown-prop:1", "unsupported CSS property"),
            ("fill:url(https://example.invalid/a.svg)", "external URL reference"),
            (r"fill:u\72l(https://example.invalid/a.svg)", "unsafe or oversized CSS"),
            ("fill:local(Helvetica)", "escaped or environment-dependent value"),
            ("fo/**/nt:700 12px Helvetica", "unsafe or oversized CSS"),
        )
        for declaration, message in unsafe_declarations:
            with self.subTest(declaration=declaration):
                self.assert_svg_rejected(
                    f"<style>.x{{{declaration}}}</style><text class=\"x\">unsafe</text>",
                    message,
                )

    def test_direct_presentation_attributes_reject_escaped_and_environment_values(self) -> None:
        unsafe_attributes = (
            r'fill="u\72l(https://example.invalid/a.svg)"',
            r'filter="u\72l(file:///C:/Windows/Fonts/arial.ttf)"',
            'fill="u/**/rl(https://example.invalid/a.svg)"',
            'fill="local(Helvetica)"',
            'fill="var(--x)"',
            'stroke="file:///C:/Windows/Fonts/arial.ttf"',
            'marker-end="url(https://example.invalid/marker.svg)"',
        )
        for attribute in unsafe_attributes:
            with self.subTest(attribute=attribute):
                self.assert_svg_rejected(f"<path {attribute} d=\"M0 0L1 1\"/>")

    def test_resolved_decimal_and_hex_del_are_rejected_in_all_xml_scalars(self) -> None:
        unsafe_content = (
            "<text>&#127;</text>",
            "<text>&#x7F;</text>",
            "<g/>&#127;",
            '<text aria-labelledby="&#x7F;">unsafe</text>',
        )
        for content in unsafe_content:
            with self.subTest(content=content):
                self.assert_svg_rejected(content, "U+007F")

    def test_node_limit_rejects_before_full_tree_parser_is_called(self) -> None:
        content = "<g/>" * ATLAS.MAX_SVG_NODES
        with mock.patch.object(
            ATLAS.ET,
            "fromstring",
            side_effect=AssertionError("full-tree parser must not be reached"),
        ) as full_parse:
            self.assert_svg_rejected(content, "streaming structural limits")
        full_parse.assert_not_called()

    def test_depth_limit_rejects_before_full_tree_parser_is_called(self) -> None:
        content = "<g>" * ATLAS.MAX_SVG_DEPTH + "safe" + "</g>" * ATLAS.MAX_SVG_DEPTH
        with mock.patch.object(
            ATLAS.ET,
            "fromstring",
            side_effect=AssertionError("full-tree parser must not be reached"),
        ) as full_parse:
            self.assert_svg_rejected(content, "streaming structural limits")
        full_parse.assert_not_called()

    def test_formula_multiplication_survives_ascii_normalization_as_star(self) -> None:
        expectations = {
            "docs/media/voxel-native-hero.svg": (
                (b"16 * 2", b"16 | 2"),
                (b"30 * Delta", b"30 | Delta"),
            ),
            "docs/media/planetary-budget-envelope.svg": (
                (b"16 * 2", b"16 | 2"),
                (b"30 * Delta", b"30 | Delta"),
            ),
            "docs/media/city-site-score.svg": (
                (b"0.35 * proximity", b"0.35 | proximity"),
                (b"0.65 * frontage", b"0.65 | frontage"),
                (b") * clamp(1 - distance", b") | clamp(1 - distance"),
                (b"0.55*clamp", b"0.55 | clamp"),
                (b"0.30*clamp", b"0.30 | clamp"),
                (b"0.15*clamp", b"0.15 | clamp"),
            ),
        }
        for relative, pairs in expectations.items():
            with self.subTest(relative=relative):
                source = ATLAS.read_input_snapshot(ROOT, relative, "SVG asset")
                normalized = ATLAS.validate_passive_svg(source)
                for expected, corrupted in pairs:
                    self.assertIn(expected, normalized)
                    self.assertNotIn(corrupted, normalized)

    def test_repository_formula_labels_are_plain_ascii_single_text_nodes(self) -> None:
        exact_labels = {
            "docs/media/voxel-native-hero.svg": {
                "Delta_l = 16 * 2^l m",
                "R_l = 30 * Delta_l",
                "L_inf radius = 15.36 km",
            },
            "docs/media/planetary-budget-envelope.svg": {
                "Delta_l = 16 * 2^l m | ||(x,z)||_inf <= R_l = 30 * Delta_l",
                "B_mesh(V, I) = 48V + 4I",
            },
        }
        for relative in ATLAS.REQUIRED_ASSETS:
            with self.subTest(relative=relative):
                source = ATLAS.read_input_snapshot(ROOT, relative, "SVG asset")
                normalized = ATLAS.validate_passive_svg(source)
                self.assertNotIn(b"<tspan", normalized)
                root = ATLAS.ET.fromstring(normalized)
                text_nodes = [
                    element
                    for element in root.iter()
                    if ATLAS.xml_local_name(element.tag) == "text"
                ]
                self.assertTrue(all(len(element) == 0 for element in text_nodes))
                observed = {element.text or "" for element in text_nodes}
                for label in exact_labels.get(relative, set()):
                    self.assertIn(label, observed)

        self.assert_svg_rejected(
            '<text>Delta_l = 2<tspan baseline-shift="super">l</tspan></text>',
            "unsupported <tspan>",
        )

    def test_city_alignment_label_has_a_fixed_right_safe_area(self) -> None:
        source = ATLAS.read_input_snapshot(
            ROOT, "docs/media/city-site-score.svg", "SVG asset"
        )
        ATLAS.validate_passive_svg(source)
        self.assertIn(
            '<text x="365" y="31" class="micro">road_anchor_alignment</text>',
            source.text,
        )
        courier_advance = len("road_anchor_alignment") * 13 * 0.6
        self.assertLessEqual(31 + 365 + courier_advance, 584 - 20)

    def test_research_card_text_has_high_contrast_against_every_card(self) -> None:
        source = ATLAS.read_input_snapshot(
            ROOT, "docs/media/research-routes.svg", "SVG asset"
        )
        ATLAS.validate_passive_svg(source)
        for token in (
            ".cardTitle{font-family:Helvetica;font-size:22px;font-weight:bold;fill:#102a3a}",
            ".cardText{font-family:Helvetica;font-size:16px;font-weight:normal;fill:#17384a}",
            ".cardMicro{font-family:Courier;font-size:14px;font-weight:bold;letter-spacing:1.35px;fill:#102a3a}",
        ):
            self.assertIn(token, source.text)

        def luminance(color: str) -> float:
            channels = [int(color[index : index + 2], 16) / 255 for index in (1, 3, 5)]
            linear = [
                value / 12.92
                if value <= 0.04045
                else ((value + 0.055) / 1.055) ** 2.4
                for value in channels
            ]
            return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]

        for background in ("#dff4fa", "#e2f5e9", "#eee7fb"):
            for foreground in ("#102a3a", "#17384a"):
                light, dark = sorted(
                    (luminance(background), luminance(foreground)), reverse=True
                )
                self.assertGreaterEqual((light + 0.05) / (dark + 0.05), 7.0)

    def test_svglib_base_family_weight_style_matrix_never_resolves_external_fonts(self) -> None:
        try:
            dependencies = ATLAS.load_pdf_dependencies()
            import svglib.fonts as svg_fonts
        except (ATLAS.AtlasBuildError, ImportError) as error:
            self.skipTest(str(error))

        forbidden: Callable[..., object] = mock.Mock(
            side_effect=AssertionError("dynamic font lookup/registration must not run")
        )
        expected_names = {
            ("Courier", "normal", "normal"): "Courier",
            ("Courier", "normal", "italic"): "Courier-Oblique",
            ("Courier", "normal", "oblique"): "Courier-Oblique",
            ("Courier", "bold", "normal"): "Courier-Bold",
            ("Courier", "bold", "italic"): "Courier-BoldOblique",
            ("Courier", "bold", "oblique"): "Courier-BoldOblique",
            ("Helvetica", "normal", "normal"): "Helvetica",
            ("Helvetica", "normal", "italic"): "Helvetica-Oblique",
            ("Helvetica", "normal", "oblique"): "Helvetica-Oblique",
            ("Helvetica", "bold", "normal"): "Helvetica-Bold",
            ("Helvetica", "bold", "italic"): "Helvetica-BoldOblique",
            ("Helvetica", "bold", "oblique"): "Helvetica-BoldOblique",
        }
        with (
            mock.patch.object(svg_fonts.FontMap, "register_font", forbidden),
            mock.patch.object(svg_fonts.FontMap, "use_fontconfig", forbidden),
            mock.patch.object(svg_fonts, "TTFont", forbidden),
            mock.patch.object(svg_fonts.subprocess, "Popen", forbidden),
            mock.patch.object(svg_fonts.subprocess, "run", forbidden),
            mock.patch.object(svg_fonts.subprocess, "check_output", forbidden),
        ):
            for (family, weight, style), expected in expected_names.items():
                with self.subTest(family=family, weight=weight, style=style):
                    fixture = snapshot(
                        svg_with(
                            f"<style>.x{{font-size:12px;font-weight:{weight};"
                            f"font-style:{style};fill:#fff}}</style>"
                            f'<g font-family="{family}">'
                            '<text class="x" x="1" y="5">B</text></g>'
                        )
                    )
                    drawing = ATLAS.parse_svg_drawing(fixture, dependencies["svg2rlg"])
                    pending = [drawing]
                    observed: list[str] = []
                    while pending:
                        item = pending.pop()
                        font_name = getattr(item, "fontName", None)
                        if font_name is not None:
                            observed.append(str(font_name))
                        pending.extend(getattr(item, "contents", ()) or ())
                    self.assertIn(expected, observed)
                    self.assertTrue(
                        set(observed).issubset(ATLAS.CANONICAL_SVG_FONT_FAMILIES)
                    )

    def test_all_repository_svgs_convert_without_external_font_resolution(self) -> None:
        try:
            dependencies = ATLAS.load_pdf_dependencies()
            import svglib.fonts as svg_fonts
        except (ATLAS.AtlasBuildError, ImportError) as error:
            self.skipTest(str(error))

        forbidden: Callable[..., object] = mock.Mock(
            side_effect=AssertionError("external font resolution must not run")
        )
        observed: list[str] = []
        text_by_asset: dict[str, set[str]] = {}
        with (
            mock.patch.object(svg_fonts.FontMap, "register_font", forbidden),
            mock.patch.object(svg_fonts.FontMap, "use_fontconfig", forbidden),
            mock.patch.object(svg_fonts, "TTFont", forbidden),
            mock.patch.object(svg_fonts.subprocess, "Popen", forbidden),
            mock.patch.object(svg_fonts.subprocess, "run", forbidden),
            mock.patch.object(svg_fonts.subprocess, "check_output", forbidden),
        ):
            for relative in ATLAS.REQUIRED_ASSETS:
                source = ATLAS.read_input_snapshot(ROOT, relative, "SVG asset")
                drawing = ATLAS.parse_svg_drawing(source, dependencies["svg2rlg"])
                pending = [drawing]
                asset_text: set[str] = set()
                while pending:
                    item = pending.pop()
                    font_name = getattr(item, "fontName", None)
                    if font_name is not None:
                        observed.append(str(font_name))
                    text_value = getattr(item, "text", None)
                    if text_value is not None:
                        rendered_text = str(text_value)
                        asset_text.add(rendered_text)
                        if relative == "docs/media/research-routes.svg":
                            bounds = item.getBounds()
                            if rendered_text.startswith("Question:"):
                                self.assertLessEqual(bounds[2], 436)
                            if rendered_text in {
                                "HEIGHT MAP -> HORIZON -> ERROR / LATENCY",
                                "NEEDLES -> CONES -> BOUGHS -> BUDGET",
                                "INPUT -> STAGES -> INSPECT / ABLATE",
                            }:
                                self.assertLessEqual(bounds[2], 408)
                    pending.extend(getattr(item, "contents", ()) or ())
                text_by_asset[relative] = asset_text
        self.assertTrue(observed)
        self.assertIn("Helvetica-Bold", observed)
        self.assertIn("Courier-Bold", observed)
        self.assertTrue(set(observed).issubset(ATLAS.CANONICAL_SVG_FONT_FAMILIES))
        self.assertIn(
            "Delta_l = 16 * 2^l m | ||(x,z)||_inf <= R_l = 30 * Delta_l",
            text_by_asset["docs/media/planetary-budget-envelope.svg"],
        )
        self.assertIn(
            "B_mesh(V, I) = 48V + 4I",
            text_by_asset["docs/media/planetary-budget-envelope.svg"],
        )
        self.assertIn(
            "Delta_l = 16 * 2^l m",
            text_by_asset["docs/media/voxel-native-hero.svg"],
        )


class PassivePdfObjectSafetyTests(unittest.TestCase):
    def test_reportlab_page_transition_defaults_are_stripped_before_serialization(self) -> None:
        page = type("Page", (), {})()
        page.Trans = {}
        page.Dur = 3
        ATLAS.strip_implicit_reportlab_page_interaction(page)
        self.assertNotIn("Trans", vars(page))
        self.assertNotIn("Dur", vars(page))

    def test_new_active_action_type_and_page_keys_are_rejected(self) -> None:
        active_objects = (
            {"/S": "/Named"},
            {"/S": "/GoTo3DView"},
            {"/Type": "/PS"},
            {"/Subtype": "/PS"},
            {"/Trans": {}},
            {"/Dur": 1},
            {"/Outlines": {}},
        )
        for active in active_objects:
            with self.subTest(active=active):
                with self.assertRaises(ATLAS.AtlasBuildError):
                    ATLAS.validate_no_active_pdf_objects({"/Nested": [active]})

    def test_passive_uri_and_ordinary_xobject_shapes_remain_allowed(self) -> None:
        uri = sorted(ATLAS.OFFICIAL_URIS)[0]
        ATLAS.validate_no_active_pdf_objects(
            {
                "/Type": "/XObject",
                "/Subtype": "/Link",
                "/A": {"/S": "/URI", "/URI": uri},
            }
        )

    def test_every_uri_action_is_globally_allowlisted_and_has_no_chain(self) -> None:
        approved = sorted(ATLAS.OFFICIAL_URIS)[0]
        ATLAS.validate_no_active_pdf_objects({"/CatalogSideAction": {"/S": "/URI", "/URI": approved}})
        unsafe_actions = (
            {"/S": "/URI", "/URI": "https://example.invalid/"},
            {"/S": "/URI"},
            {"/S": "/URI", "/URI": [approved]},
            {"/S": "/URI", "/URI": approved, "/Next": None},
        )
        for action in unsafe_actions:
            with self.subTest(action=action):
                with self.assertRaises(ATLAS.AtlasBuildError):
                    ATLAS.validate_no_active_pdf_objects({"/OutlineLike": action})

    def test_only_canonical_type1_winansi_pdf_fonts_are_allowed(self) -> None:
        ATLAS.validate_no_active_pdf_objects(
            {
                "/Type": "/Font",
                "/BaseFont": "/Helvetica",
                "/Subtype": "/Type1",
                "/Encoding": "/WinAnsiEncoding",
            }
        )
        unsafe_fonts = (
            ("/Symbol", "/Type1", "/WinAnsiEncoding", {}),
            ("/ArialUnicode", "/Type1", "/WinAnsiEncoding", {}),
            ("/Helvetica", "/TrueType", "/WinAnsiEncoding", {}),
            ("/Helvetica", "/Type1", "/Identity-H", {}),
            ("/Helvetica", "/Type1", "/WinAnsiEncoding", {"/ToUnicode": {}}),
        )
        for base_font, subtype, encoding, extra in unsafe_fonts:
            with self.subTest(base_font=base_font, subtype=subtype, encoding=encoding):
                font = {
                    "/Type": "/Font",
                    "/BaseFont": base_font,
                    "/Subtype": subtype,
                    "/Encoding": encoding,
                    **extra,
                }
                with self.assertRaisesRegex(ATLAS.AtlasBuildError, "non-canonical"):
                    ATLAS.validate_no_active_pdf_objects({"/FontResource": font})

        class FakeIndirectObject:
            def __init__(self, idnum: int, value: object) -> None:
                self.idnum = idnum
                self.generation = 0
                self.value = value

            def get_object(self) -> object:
                return self.value

        identity = 100
        for font_file_key in ("/FontFile", "/FontFile2", "/FontFile3"):
            for indirect_descriptor in (False, True):
                identity += 2
                font_program = FakeIndirectObject(identity, {"/Length": 4})
                descriptor_value = {
                    "/Type": "/FontDescriptor",
                    font_file_key: font_program,
                }
                descriptor: object = (
                    FakeIndirectObject(identity + 1, descriptor_value)
                    if indirect_descriptor
                    else descriptor_value
                )
                canonical_outer_font = {
                    "/Type": "/Font",
                    "/BaseFont": "/Helvetica",
                    "/Subtype": "/Type1",
                    "/Encoding": "/WinAnsiEncoding",
                    "/FontDescriptor": descriptor,
                }
                with self.subTest(
                    font_file_key=font_file_key,
                    indirect_descriptor=indirect_descriptor,
                ):
                    with self.assertRaisesRegex(
                        ATLAS.AtlasBuildError, f"forbidden active key {font_file_key}"
                    ):
                        ATLAS.validate_no_active_pdf_objects(
                            {"/FontResource": canonical_outer_font}
                        )

    def test_pdf_id_hex_preserves_pypdf_original_bytes(self) -> None:
        try:
            from pypdf.generic import ByteStringObject, TextStringObject, create_string_object
        except ImportError as error:
            self.skipTest(str(error))

        decoded = create_string_object(b"\x1aA")
        self.assertIsInstance(decoded, TextStringObject)
        self.assertIn("\u02c6", decoded)
        self.assertEqual(decoded.original_bytes, b"\x1aA")
        self.assertEqual(ATLAS.pdf_id_hex(decoded, TextStringObject), "1A41")

        ordinary_bytes = ByteStringObject(b"\x00\xff")
        self.assertEqual(ATLAS.pdf_id_hex(ordinary_bytes, TextStringObject), "00FF")
        self.assertEqual(ATLAS.pdf_id_hex(b"\x80\x01", TextStringObject), "8001")

        class ForgedOriginalBytes:
            original_bytes = b"forged"

        class ForgedString(str):
            @property
            def original_bytes(self) -> bytes:
                return b"forged-string"

        ForgedMetadataTextString = type(
            "TextStringObject",
            (str,),
            {
                "__module__": "pypdf.lookalike",
                "original_bytes": property(lambda self: b"forged-metadata"),
            },
        )
        for unsupported in (
            "\u02c6",
            2,
            [0, 1],
            object(),
            ForgedOriginalBytes(),
            ForgedString("lookalike"),
            ForgedMetadataTextString("lookalike"),
        ):
            with self.subTest(unsupported=type(unsupported).__name__):
                with self.assertRaisesRegex(ATLAS.AtlasBuildError, "authoritative bytes"):
                    ATLAS.pdf_id_hex(unsupported, TextStringObject)


class ReleaseValidationCliTests(unittest.TestCase):
    def test_validate_release_is_exclusive_and_rejects_nondefault_output(self) -> None:
        for conflicting_flag in (
            "--force",
            "--no-clobber",
            "--check-only",
            "--verify-determinism",
        ):
            with self.subTest(conflicting_flag=conflicting_flag):
                with mock.patch("sys.stderr", new=io.StringIO()):
                    with self.assertRaises(SystemExit) as caught:
                        ATLAS.parse_args(["--validate-release", conflicting_flag])
                self.assertEqual(caught.exception.code, 2)

        with mock.patch("sys.stderr", new=io.StringIO()):
            with self.assertRaises(SystemExit) as caught:
                ATLAS.parse_args(
                    ["--validate-release", "--output", "tmp/not-the-release.pdf"]
                )
        self.assertEqual(caught.exception.code, 2)

        args = ATLAS.parse_args(
            ["--validate-release", "--output", str(ATLAS.DEFAULT_OUTPUT)]
        )
        self.assertTrue(args.validate_release)

    def test_document_identity_helper_binds_all_identity_inputs(self) -> None:
        builder_snapshot = ATLAS.InputSnapshot(
            relative=ATLAS.BUILDER_SOURCE,
            path=BUILDER_PATH,
            data=b"fixture",
            text="fixture",
            sha256="1" * 64,
        )
        identity = ATLAS.compute_atlas_document_identity(
            {ATLAS.BUILDER_SOURCE: builder_snapshot},
            "0" * 64,
            "fixture=1",
        )
        self.assertEqual(identity.fingerprint, "0" * 64)
        self.assertEqual(identity.builder_sha, "1" * 64)
        self.assertEqual(identity.toolchain_identity, "fixture=1")
        self.assertEqual(identity.document_id_hex, "7684CBB7658E19AB0CA7EAF70217DF77")

    def test_canonical_release_reader_has_one_fixed_path_and_cap(self) -> None:
        expected_path = ATLAS.lexical_absolute(ROOT / ATLAS.CANONICAL_RELEASE_PDF)
        expected_data = b"%PDF-read-only-fixture"
        with mock.patch.object(
            ATLAS,
            "read_stable_bounded_bytes",
            return_value=expected_data,
        ) as bounded_reader:
            observed_path, observed_data = ATLAS.read_canonical_release_pdf(ROOT)
        self.assertEqual(observed_path, expected_path)
        self.assertIs(observed_data, expected_data)
        bounded_reader.assert_called_once_with(
            expected_path,
            ROOT,
            byte_limit=ATLAS.MAX_PDF_BYTES,
            label="canonical release PDF",
        )

    def test_stable_bounded_reader_is_read_only_and_enforces_cap(self) -> None:
        expected = BUILDER_PATH.read_bytes()
        actual_open = ATLAS.os.open
        observed_flags: list[int] = []
        forbidden_flags = (
            ATLAS.os.O_WRONLY
            | ATLAS.os.O_RDWR
            | ATLAS.os.O_CREAT
            | ATLAS.os.O_TRUNC
        )

        def tracked_open(path: Path, flags: int) -> int:
            self.assertEqual(flags & forbidden_flags, 0)
            observed_flags.append(flags)
            return actual_open(path, flags)

        with mock.patch.object(ATLAS.os, "open", side_effect=tracked_open):
            observed = ATLAS.read_stable_bounded_bytes(
                BUILDER_PATH,
                ROOT,
                byte_limit=len(expected),
                label="read-only fixture",
            )
            with self.assertRaisesRegex(ATLAS.AtlasBuildError, "input cap"):
                ATLAS.read_stable_bounded_bytes(
                    BUILDER_PATH,
                    ROOT,
                    byte_limit=len(expected) - 1,
                    label="read-only fixture",
                )
        self.assertEqual(observed, expected)
        self.assertEqual(len(observed_flags), 2)

    def test_stable_bounded_reader_rejects_missing_and_outside_paths_without_opening(self) -> None:
        missing = ROOT / "tools" / "artifacts" / ".missing-release-reader-fixture.pdf"
        self.assertFalse(missing.exists())
        with self.assertRaisesRegex(ATLAS.AtlasBuildError, "missing required"):
            ATLAS.read_stable_bounded_bytes(
                missing,
                ROOT,
                byte_limit=ATLAS.MAX_PDF_BYTES,
                label="missing fixture",
            )
        self.assertFalse(missing.exists())

        outside = ROOT.parent / "outside-release-reader-fixture.pdf"
        with mock.patch.object(
            ATLAS.os,
            "open",
            side_effect=AssertionError("an outside path must be rejected before open"),
        ):
            with self.assertRaisesRegex(ATLAS.AtlasBuildError, "escapes its allowed root"):
                ATLAS.read_stable_bounded_bytes(
                    outside,
                    ROOT,
                    byte_limit=ATLAS.MAX_PDF_BYTES,
                    label="outside fixture",
                )

    def test_stable_bounded_reader_rejects_during_read_change(self) -> None:
        source_stat = ATLAS.os.stat(BUILDER_PATH, follow_symlinks=False)

        def stat_view(mtime_ns: int) -> object:
            value = mock.Mock()
            value.st_mode = source_stat.st_mode
            value.st_file_attributes = getattr(source_stat, "st_file_attributes", 0)
            value.st_dev = source_stat.st_dev
            value.st_ino = source_stat.st_ino
            value.st_size = source_stat.st_size
            value.st_mtime_ns = mtime_ns
            value.st_ctime_ns = source_stat.st_ctime_ns
            return value

        before = stat_view(source_stat.st_mtime_ns)
        after = stat_view(source_stat.st_mtime_ns + 1)
        with mock.patch.object(ATLAS.os, "fstat", side_effect=(before, after)):
            with self.assertRaisesRegex(ATLAS.AtlasBuildError, "changed while"):
                ATLAS.read_stable_bounded_bytes(
                    BUILDER_PATH,
                    ROOT,
                    byte_limit=ATLAS.MAX_PDF_BYTES,
                    label="changing fixture",
                )

    def test_validate_release_main_bypasses_output_and_publication(self) -> None:
        builder_data = b"bound builder fixture"
        builder_snapshot = ATLAS.InputSnapshot(
            relative=ATLAS.BUILDER_SOURCE,
            path=BUILDER_PATH,
            data=builder_data,
            text=builder_data.decode("ascii"),
            sha256=hashlib.sha256(builder_data).hexdigest(),
        )
        fingerprint = "a" * 64
        release_data = b"%PDF-captured-release-fixture"
        release_path = ATLAS.lexical_absolute(ROOT / ATLAS.CANONICAL_RELEASE_PDF)
        text_string_type = type("ExactTextString", (str,), {})
        dependencies = {
            "PdfReader": object(),
            "TextStringObject": text_string_type,
            "svg2rlg": object(),
            "toolchain_identity": "python=fixture",
        }

        with (
            mock.patch.object(
                ATLAS,
                "validate_inputs",
                return_value=({ATLAS.BUILDER_SOURCE: builder_snapshot}, fingerprint),
            ),
            mock.patch.object(ATLAS, "load_pdf_dependencies", return_value=dependencies),
            mock.patch.object(ATLAS, "validate_svg_snapshots") as validate_svgs,
            mock.patch.object(
                ATLAS,
                "read_canonical_release_pdf",
                return_value=(release_path, release_data),
            ),
            mock.patch.object(ATLAS, "validate_built_pdf", return_value=15) as validate_pdf,
            mock.patch.object(
                ATLAS,
                "validate_output",
                side_effect=AssertionError("release validation must not select an output"),
            ),
            mock.patch.object(
                ATLAS,
                "build_pdf_bytes",
                side_effect=AssertionError("release validation must not build a PDF"),
            ),
            mock.patch.object(
                ATLAS,
                "prepare_output_parent",
                side_effect=AssertionError("release validation must not prepare output"),
            ),
            mock.patch.object(
                ATLAS,
                "write_validated_temp",
                side_effect=AssertionError("release validation must not write a temporary PDF"),
            ),
            mock.patch.object(
                ATLAS,
                "publish",
                side_effect=AssertionError("release validation must not publish a PDF"),
            ),
            mock.patch("builtins.print") as print_output,
        ):
            result = ATLAS.main(["--validate-release"])

        self.assertEqual(result, 0)
        validate_svgs.assert_called_once()
        validated_identity = validate_pdf.call_args.args[3]
        self.assertEqual(validated_identity.fingerprint, fingerprint)
        self.assertEqual(validated_identity.builder_sha, builder_snapshot.sha256)
        self.assertEqual(validated_identity.toolchain_identity, "python=fixture")
        printed = " ".join(
            str(argument)
            for call in print_output.call_args_list
            for argument in call.args
        )
        self.assertIn(str(release_path), printed)
        self.assertIn(hashlib.sha256(release_data).hexdigest(), printed)
        self.assertIn(builder_snapshot.sha256, printed)
        self.assertIn("pages 15", printed)


class AtlasVisibleTextRegressionTests(unittest.TestCase):
    def test_rollback_labels_are_short_human_readable_tokens(self) -> None:
        self.assertEqual(ATLAS.HYDRO_ROLLBACK_LABEL, "Hydro gate off")
        self.assertEqual(ATLAS.COHORT_ROLLBACK_LABEL, "Cohort gate off")
        self.assertNotIn("_", ATLAS.HYDRO_ROLLBACK_LABEL + ATLAS.COHORT_ROLLBACK_LABEL)

    def test_road_grade_formula_keeps_divisor_and_clamp_arguments_together(self) -> None:
        lines = ATLAS.ROAD_GRADE_FIT_FORMULA.splitlines()
        self.assertIn("        max(height_range-18,0)/34,0,1),", lines)
        self.assertNotIn("/3\n4", ATLAS.ROAD_GRADE_FIT_FORMULA)
        self.assertLessEqual(max(map(len, lines)), 40)


class BuilderSourceBindingTests(unittest.TestCase):
    def test_input_identity_requires_byte_bound_builder_source(self) -> None:
        with self.assertRaisesRegex(ATLAS.AtlasBuildError, "byte-bound builder source"):
            ATLAS.validate_inputs(ROOT)

    def test_input_identity_never_reopens_bound_builder_path(self) -> None:
        bound = BUILDER_PATH.read_bytes()
        original_reader = ATLAS.read_input_snapshot

        def guarded_reader(root: Path, relative: str, label: str) -> object:
            if relative == ATLAS.BUILDER_SOURCE:
                raise AssertionError("bound builder path must never be reopened")
            return original_reader(root, relative, label)

        with mock.patch.object(ATLAS, "read_input_snapshot", side_effect=guarded_reader):
            files, fingerprint = ATLAS.validate_inputs(ROOT, bound)
        self.assertIs(files[ATLAS.BUILDER_SOURCE].data, bound)
        self.assertEqual(files[ATLAS.BUILDER_SOURCE].sha256, hashlib.sha256(bound).hexdigest())
        self.assertRegex(fingerprint, r"^[0-9a-f]{64}$")

    def test_copied_builder_path_is_rejected_before_snapshot_labeling(self) -> None:
        copied = ROOT / "tools" / "artifacts" / "copied_atlas_builder.py"
        with mock.patch.object(ATLAS, "__file__", str(copied)):
            with self.assertRaisesRegex(ATLAS.AtlasBuildError, "canonical repository path"):
                ATLAS.bound_builder_snapshot(ROOT, b"copied bytes")


class ToolchainIdentityTests(unittest.TestCase):
    def test_missing_or_ambiguous_dependency_versions_fail_closed(self) -> None:
        for value in (None, "", "unknown", "1.0+unknown", "1.0;other=2"):
            with self.subTest(value=value):
                with self.assertRaises(ATLAS.AtlasBuildError):
                    ATLAS.canonical_toolchain_version("fixture", value)
        for value in (None, (), (1, -1), (1, "2"), [1, 2]):
            with self.subTest(compiled=value):
                with self.assertRaises(ATLAS.AtlasBuildError):
                    ATLAS.canonical_compiled_version("fixture", value)

    def test_svg_parser_dependency_identity_is_complete_and_sorted(self) -> None:
        try:
            dependencies = ATLAS.load_pdf_dependencies()
            from pypdf.generic import TextStringObject
        except ATLAS.AtlasBuildError as error:
            self.skipTest(str(error))
        self.assertIs(dependencies["TextStringObject"], TextStringObject)
        components = dependencies["toolchain_identity"].split(";")
        names = [component.split("=", 1)[0] for component in components]
        self.assertEqual(names, sorted(names))
        self.assertEqual(
            set(names),
            {
                "cssselect2",
                "libxml2-compiled",
                "libxml2-runtime",
                "libxslt-compiled",
                "libxslt-runtime",
                "lxml",
                "pypdf",
                "python",
                "reportlab",
                "svglib",
                "tinycss2",
                "zlib-compiled",
                "zlib-runtime",
            },
        )
        for component in components:
            name, version = component.split("=", 1)
            self.assertTrue(version, name)
            self.assertNotIn("unknown", version.lower())
            self.assertRegex(version, r"^[0-9A-Za-z][0-9A-Za-z.+_-]*$")


if __name__ == "__main__":
    unittest.main()
