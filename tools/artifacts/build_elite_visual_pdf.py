#!/usr/bin/env python3
"""Build a PDF dossier from one explicit canonical QA evidence manifest."""

from __future__ import annotations

import argparse
import json
import os
import uuid
from pathlib import Path
from typing import Any, Sequence

from evidence_manifest_consumer import (
    CanonicalEvidence,
    EvidenceContractError,
    iter_claims,
    iter_issues,
    load_canonical_evidence,
    publish_no_clobber,
    validate_output_path,
    validation_summary,
    verified_screenshots,
)


INK = NAVY = TEAL = CREAM = PALE = LINE = MUTED = WHITE = None
PAGE_W = PAGE_H = LEFT = RIGHT = TOP = BOTTOM = CONTENT_W = 0
FONT = "Helvetica"
FONT_BOLD = "Helvetica-Bold"
STYLES: dict[str, Any] = {}
MAX_ARTIFACT_ROWS = 300
PDF_CONTENT_WIDTH_MM = 174
RUN_TABLE_HEADERS = (
    "Explicit run",
    "Build / world",
    "Viewport",
    "Route",
    "Route-only frame time",
    "Far surface",
    "PNGs",
)
RUN_TABLE_WIDTHS_MM = (27, 23, 22, 22, 39, 25, 16)
BUDGET_TABLE_HEADERS = (
    "Run",
    "Entities",
    "Vertices",
    "Indices",
    "Mesh bytes",
    "Cache live / peak",
    "Decision",
)
BUDGET_TABLE_WIDTHS_MM = (17, 22, 25, 25, 25, 41, 19)
BUDGET_RUNS_PER_PAGE = 8
CLAIM_TABLE_HEADERS = ("Scope", "Class", "Claim", "Evidence paths")
CLAIM_TABLE_WIDTHS_MM = (31, 18, 61, 64)
# Calibrated for the current A4 frame and 6.9 pt / 8.7 pt ledger typography.
CLAIM_ROWS_PER_PAGE = 8
FILE_IDENTITY_HEADERS = ("Kind", "Path", "Bytes", "SHA-256")
FILE_IDENTITY_WIDTHS_MM = (30, 66, 18, 60)
GENERATOR_SOURCE_DISPLAY_LABEL = "generator\u00a0source"


def load_pdf_dependencies() -> None:
    """Load bundled PDF dependencies only for a real artifact build."""

    global PILImage, colors, TA_CENTER, TA_LEFT, A4, ParagraphStyle
    global getSampleStyleSheet, mm, pdfmetrics, TTFont, BaseDocTemplate, Frame
    global Image, KeepTogether, PageBreak, PageTemplate, Paragraph, Spacer, Table, TableStyle
    global INK, NAVY, TEAL, CREAM, PALE, LINE, MUTED, WHITE
    global PAGE_W, PAGE_H, LEFT, RIGHT, TOP, BOTTOM, CONTENT_W
    global FONT, FONT_BOLD, STYLES
    try:
        from PIL import Image as _PILImage
        from reportlab.lib import colors as _colors
        from reportlab.lib.enums import TA_CENTER as _TA_CENTER, TA_LEFT as _TA_LEFT
        from reportlab.lib.pagesizes import A4 as _A4
        from reportlab.lib.styles import (
            ParagraphStyle as _ParagraphStyle,
            getSampleStyleSheet as _getSampleStyleSheet,
        )
        from reportlab.lib.units import mm as _mm
        from reportlab.pdfbase import pdfmetrics as _pdfmetrics
        from reportlab.pdfbase.ttfonts import TTFont as _TTFont
        from reportlab.platypus import (
            BaseDocTemplate as _BaseDocTemplate,
            Frame as _Frame,
            Image as _Image,
            KeepTogether as _KeepTogether,
            PageBreak as _PageBreak,
            PageTemplate as _PageTemplate,
            Paragraph as _Paragraph,
            Spacer as _Spacer,
            Table as _Table,
            TableStyle as _TableStyle,
        )
    except ImportError as error:
        raise EvidenceContractError(
            "Pillow/reportlab are unavailable; run a real build with the bundled workspace dependency runtime"
        ) from error
    PILImage = _PILImage
    colors = _colors
    TA_CENTER = _TA_CENTER
    TA_LEFT = _TA_LEFT
    A4 = _A4
    ParagraphStyle = _ParagraphStyle
    getSampleStyleSheet = _getSampleStyleSheet
    mm = _mm
    pdfmetrics = _pdfmetrics
    TTFont = _TTFont
    BaseDocTemplate = _BaseDocTemplate
    Frame = _Frame
    Image = _Image
    KeepTogether = _KeepTogether
    PageBreak = _PageBreak
    PageTemplate = _PageTemplate
    Paragraph = _Paragraph
    Spacer = _Spacer
    Table = _Table
    TableStyle = _TableStyle
    INK = colors.HexColor("#122128")
    NAVY = colors.HexColor("#173B4D")
    TEAL = colors.HexColor("#1F8A70")
    CREAM = colors.HexColor("#F5F1E8")
    PALE = colors.HexColor("#EAF3F1")
    LINE = colors.HexColor("#C9D8D5")
    MUTED = colors.HexColor("#53666E")
    WHITE = colors.white
    PAGE_W, PAGE_H = A4
    LEFT = 18 * mm
    RIGHT = 18 * mm
    TOP = 17 * mm
    BOTTOM = 17 * mm
    CONTENT_W = PAGE_W - LEFT - RIGHT
    FONT, FONT_BOLD = register_fonts()
    STYLES = make_styles()


def register_fonts() -> tuple[str, str]:
    regular = next(
        (path for path in (Path("C:/Windows/Fonts/aptos.ttf"), Path("C:/Windows/Fonts/segoeui.ttf")) if path.exists()),
        None,
    )
    bold = next(
        (path for path in (Path("C:/Windows/Fonts/aptos-bold.ttf"), Path("C:/Windows/Fonts/segoeuib.ttf")) if path.exists()),
        None,
    )
    if regular and bold:
        pdfmetrics.registerFont(TTFont("EliteSans", str(regular)))
        pdfmetrics.registerFont(TTFont("EliteSans-Bold", str(bold)))
        return "EliteSans", "EliteSans-Bold"
    return "Helvetica", "Helvetica-Bold"


def make_styles() -> dict[str, ParagraphStyle]:
    base = getSampleStyleSheet()
    return {
        "kicker": ParagraphStyle("Kicker", parent=base["Normal"], fontName=FONT_BOLD, fontSize=8, leading=10, textColor=TEAL, spaceAfter=3 * mm),
        "title": ParagraphStyle("Title", parent=base["Title"], fontName=FONT_BOLD, fontSize=27, leading=30, textColor=NAVY, alignment=TA_LEFT, spaceAfter=3 * mm),
        "subtitle": ParagraphStyle("Subtitle", parent=base["Normal"], fontName=FONT, fontSize=11, leading=15, textColor=MUTED, spaceAfter=5 * mm),
        "h1": ParagraphStyle("Heading1", parent=base["Heading1"], fontName=FONT_BOLD, fontSize=16, leading=20, textColor=NAVY, spaceBefore=4 * mm, spaceAfter=2.5 * mm, keepWithNext=True),
        "body": ParagraphStyle("Body", parent=base["BodyText"], fontName=FONT, fontSize=9.2, leading=13.2, textColor=INK, spaceAfter=2.4 * mm),
        "small": ParagraphStyle("Small", parent=base["BodyText"], fontName=FONT, fontSize=7.2, leading=9.5, textColor=MUTED),
        "caption": ParagraphStyle("Caption", parent=base["BodyText"], fontName=FONT, fontSize=7.2, leading=9.3, textColor=MUTED, alignment=TA_CENTER, spaceBefore=1.3 * mm, spaceAfter=3 * mm),
        "table_head": ParagraphStyle("TableHead", parent=base["Normal"], fontName=FONT_BOLD, fontSize=7.1, leading=8.8, textColor=WHITE),
        "table": ParagraphStyle("Table", parent=base["Normal"], fontName=FONT, fontSize=6.9, leading=8.7, textColor=INK),
        "table_bold": ParagraphStyle("TableBold", parent=base["Normal"], fontName=FONT_BOLD, fontSize=6.9, leading=8.7, textColor=INK),
        "callout_label": ParagraphStyle("CalloutLabel", parent=base["Normal"], fontName=FONT_BOLD, fontSize=8, leading=10, textColor=NAVY),
        "callout": ParagraphStyle("Callout", parent=base["Normal"], fontName=FONT, fontSize=8.3, leading=11.5, textColor=INK),
        "bullet": ParagraphStyle("Bullet", parent=base["BodyText"], fontName=FONT, fontSize=8.7, leading=12.4, leftIndent=4 * mm, firstLineIndent=-3 * mm, textColor=INK, spaceAfter=1.7 * mm),
    }


def safe_text(value: object) -> str:
    return str(value).replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def paragraph(text: object, style: str = "body") -> Paragraph:
    return Paragraph(safe_text(text), STYLES[style])


def fmt_number(value: object, digits: int = 2) -> str:
    if type(value) is int:
        return f"{value:,}"
    if type(value) is float:
        return f"{value:,.{digits}f}"
    return str(value)


def display_hash(value: str) -> str:
    return " ".join(value[index : index + 8] for index in range(0, len(value), 8))


def scaled_image(path: Path, width: float) -> Image:
    with PILImage.open(path) as source:
        pixel_width, pixel_height = source.size
    if pixel_width <= 0 or pixel_height <= 0:
        raise EvidenceContractError(f"screenshot has invalid dimensions: {path}")
    return Image(str(path), width=width, height=width * pixel_height / pixel_width)


def matrix(
    headers: Sequence[str],
    rows: Sequence[Sequence[object]],
    widths: Sequence[float],
    *,
    split_by_row: bool = True,
) -> Table:
    if len(headers) != len(widths) or abs(sum(widths) - CONTENT_W) > 0.1:
        raise EvidenceContractError("PDF table geometry must exactly fill the content width")
    data: list[list[Paragraph]] = [[paragraph(header, "table_head") for header in headers]]
    for row in rows:
        if len(row) != len(headers):
            raise EvidenceContractError("PDF table row width does not match its header")
        data.append(
            [paragraph(value, "table_bold" if index == 0 else "table") for index, value in enumerate(row)]
        )
    table = Table(
        data,
        colWidths=list(widths),
        repeatRows=1,
        hAlign="LEFT",
        splitByRow=1 if split_by_row else 0,
    )
    commands: list[tuple] = [
        ("BACKGROUND", (0, 0), (-1, 0), NAVY),
        ("VALIGN", (0, 0), (-1, -1), "MIDDLE"),
        ("LEFTPADDING", (0, 0), (-1, -1), 5),
        ("RIGHTPADDING", (0, 0), (-1, -1), 5),
        ("TOPPADDING", (0, 0), (-1, -1), 4),
        ("BOTTOMPADDING", (0, 0), (-1, -1), 4),
        ("LINEBELOW", (0, 0), (-1, -1), 0.35, LINE),
    ]
    for row_index in range(1, len(data)):
        commands.append(("BACKGROUND", (0, row_index), (-1, row_index), PALE if row_index % 2 else colors.white))
    table.setStyle(TableStyle(commands))
    return table


def callout(label: str, text: str, fill=None) -> Table:
    if fill is None:
        fill = CREAM
    table = Table(
        [[paragraph(label, "callout_label"), paragraph(text, "callout")]],
        colWidths=[35 * mm, CONTENT_W - 35 * mm],
    )
    table.setStyle(
        TableStyle(
            [
                ("BACKGROUND", (0, 0), (-1, -1), fill),
                ("BOX", (0, 0), (-1, -1), 0.8, TEAL),
                ("VALIGN", (0, 0), (-1, -1), "TOP"),
                ("LEFTPADDING", (0, 0), (-1, -1), 8),
                ("RIGHTPADDING", (0, 0), (-1, -1), 8),
                ("TOPPADDING", (0, 0), (-1, -1), 7),
                ("BOTTOMPADDING", (0, 0), (-1, -1), 7),
            ]
        )
    )
    return table


def bullets(items: Sequence[str]) -> list[Paragraph]:
    return [paragraph(f"- {item}", "bullet") for item in items]


def page_chrome(generated_date: str):
    def draw(canvas, doc) -> None:
        canvas.saveState()
        canvas.setStrokeColor(LINE)
        canvas.setLineWidth(0.5)
        canvas.line(LEFT, PAGE_H - 10 * mm, PAGE_W - RIGHT, PAGE_H - 10 * mm)
        canvas.setFont(FONT_BOLD, 7)
        canvas.setFillColor(NAVY)
        canvas.drawString(LEFT, PAGE_H - 7.5 * mm, "VOXEL-NATIVE / CANONICAL QA EVIDENCE")
        canvas.setFont(FONT, 7)
        canvas.setFillColor(MUTED)
        canvas.drawRightString(PAGE_W - RIGHT, 8 * mm, f"{generated_date}  |  page {doc.page}")
        canvas.restoreState()

    return draw


def run_rows(evidence: CanonicalEvidence) -> list[list[str]]:
    rows: list[list[str]] = []
    for run in evidence.runs:
        observations = run["raw_observations"]
        identity = observations["run_identity"]
        viewport = observations["viewport"]
        route = observations["route"]
        frame = observations["route_frame_times"]
        planetary = observations["planetary_streaming"]
        rows.append(
            [
                run["input_path"],
                f"{identity['build_profile']} / {identity.get('world_profile', 'unrecorded')}",
                f"{viewport['physical_width']}x{viewport['physical_height']} @ {fmt_number(viewport['dpi_percent'], 0)}%",
                f"{route['route_focus']} / {fmt_number(route['requested_route_distance_m'], 0)} m",
                f"n={fmt_number(frame['sample_count'], 0)}; p50 {fmt_number(frame['median_ms'])}; p95 {fmt_number(frame['p95_ms'])}; p99 {fmt_number(frame['p99_ms'])}; max {fmt_number(frame['max_ms'])} ms",
                f"{planetary['telemetry']['surface_material_mode']} / {planetary['live']['profile']}",
                str(len(observations["screenshots"]["referenced_files"])),
            ]
        )
    return rows


def budget_ratio(live: int, budget: int) -> str:
    usage = f"{live / budget:.1%}" if budget else "0.0%" if live == 0 else "undefined"
    return f"{fmt_number(live, 0)} / {fmt_number(budget, 0)} ({usage})"


def budget_run_rows(evidence: CanonicalEvidence) -> list[list[str]]:
    """Serialize all six hard-budget observations into one bounded row per run."""

    rows: list[list[str]] = []
    for index, run in enumerate(evidence.runs, start=1):
        planetary = run["raw_observations"]["planetary_streaming"]
        live = planetary["live"]
        budgets = planetary["budgets"]
        peak = int(planetary["telemetry"]["peak_live_sample_cache_bytes"])
        cache_budget = int(budgets["budget_sample_cache_bytes"])
        rows.append(
            [
                f"Run {index:02d}",
                budget_ratio(int(live["resident_entities"]), int(budgets["budget_entities"])),
                budget_ratio(int(live["resident_vertices"]), int(budgets["budget_vertices"])),
                budget_ratio(int(live["resident_indices"]), int(budgets["budget_indices"])),
                budget_ratio(int(live["resident_mesh_bytes"]), int(budgets["budget_mesh_bytes"])),
                "live "
                + budget_ratio(int(live["live_sample_cache_bytes"]), cache_budget)
                + "; peak "
                + budget_ratio(peak, cache_budget),
                "Passed",
            ]
        )
    return rows


def plan_budget_pages(
    run_count: int, maximum_per_page: int = BUDGET_RUNS_PER_PAGE
) -> tuple[int, ...]:
    """Return balanced page populations without a final singleton run."""

    if type(run_count) is not int or run_count < 1:
        raise ValueError("budget pagination requires a positive integer run count")
    if type(maximum_per_page) is not int or maximum_per_page < 3:
        raise ValueError("budget pagination requires a per-page maximum of at least three")
    page_count = (run_count + maximum_per_page - 1) // maximum_per_page
    base, remainder = divmod(run_count, page_count)
    page_sizes = tuple(base + (1 if index < remainder else 0) for index in range(page_count))
    if page_count > 1 and min(page_sizes) < 2:
        raise EvidenceContractError("budget pagination would create an orphan run page")
    return page_sizes


def budget_flowables(evidence: CanonicalEvidence) -> list[Any]:
    """Build indivisible, balanced budget tables for deterministic pagination."""

    rows = budget_run_rows(evidence)
    page_sizes = plan_budget_pages(len(rows))
    flowables: list[Any] = []
    start = 0
    for page_index, page_size in enumerate(page_sizes):
        if page_index:
            flowables.append(PageBreak())
        page_rows = rows[start : start + page_size]
        start += page_size
        heading = "Serialized hard-budget evidence"
        if len(page_sizes) > 1:
            heading += f" ({page_index + 1}/{len(page_sizes)})"
        block: list[Any] = [paragraph(heading, "h1")]
        if page_index == 0:
            block.append(
                paragraph(
                    "Run labels follow the explicit manifest order shown above. Each cell records live or peak / budget (usage). These like-for-like checks do not imply visual acceptance or a causal performance gain."
                )
            )
        block.append(
            matrix(
                BUDGET_TABLE_HEADERS,
                page_rows,
                [width * mm for width in BUDGET_TABLE_WIDTHS_MM],
                split_by_row=False,
            )
        )
        flowables.append(KeepTogether(block))
    return flowables


def claim_rows(claims: Sequence[tuple[str, dict[str, Any]]]) -> list[list[str]]:
    """Serialize every claim exactly once in the explicit iterator order."""

    return [
        [
            scope,
            claim["classification"],
            claim["statement"],
            "\n".join(claim["evidence"]) or "None",
        ]
        for scope, claim in claims
    ]


def plan_claim_pages(
    claim_count: int, maximum_per_page: int = CLAIM_ROWS_PER_PAGE
) -> tuple[int, ...]:
    """Balance claim continuations so the final page is not underfilled."""

    if type(claim_count) is not int or claim_count < 1:
        raise ValueError("claim pagination requires a positive integer claim count")
    if type(maximum_per_page) is not int or maximum_per_page < 3:
        raise ValueError("claim pagination requires a per-page maximum of at least three")
    page_count = (claim_count + maximum_per_page - 1) // maximum_per_page
    base, remainder = divmod(claim_count, page_count)
    page_sizes = tuple(base + (1 if index < remainder else 0) for index in range(page_count))
    if page_count > 1 and min(page_sizes) < 2:
        raise EvidenceContractError("claim pagination would create an orphan continuation page")
    return page_sizes


def balanced_claim_chunks(
    rows: Sequence[Sequence[object]],
    maximum_per_page: int = CLAIM_ROWS_PER_PAGE,
) -> list[list[Sequence[object]]]:
    """Partition claim rows completely, once, and in deterministic order."""

    page_sizes = plan_claim_pages(len(rows), maximum_per_page)
    chunks: list[list[Sequence[object]]] = []
    start = 0
    for page_size in page_sizes:
        chunks.append(list(rows[start : start + page_size]))
        start += page_size
    if start != len(rows):
        raise EvidenceContractError("claim pagination did not consume every row exactly once")
    return chunks


def claim_flowables(claims: Sequence[tuple[str, dict[str, Any]]]) -> list[Any]:
    """Build balanced, indivisible claim-ledger pages with repeated headers."""

    chunks = balanced_claim_chunks(claim_rows(claims))
    flowables: list[Any] = []
    for page_index, chunk in enumerate(chunks):
        if page_index:
            flowables.append(PageBreak())
        heading = "Claim ledger"
        if len(chunks) > 1:
            heading += f" ({page_index + 1}/{len(chunks)})"
        block = [
            paragraph(heading, "h1"),
            matrix(
                CLAIM_TABLE_HEADERS,
                chunk,
                [width * mm for width in CLAIM_TABLE_WIDTHS_MM],
                split_by_row=False,
            ),
        ]
        flowables.append(KeepTogether(block))
    return flowables


def display_file_kind(kind: str) -> str:
    """Return a readable label without mutating the semantic manifest kind."""

    if kind == "generator_source":
        return GENERATOR_SOURCE_DISPLAY_LABEL
    if kind in {"report", "screenshot"}:
        return kind
    raise EvidenceContractError(f"unsupported file identity kind: {kind}")


def file_identity_rows(evidence: CanonicalEvidence) -> list[list[str]]:
    return [
        [
            display_file_kind(record["kind"]),
            record["path"],
            fmt_number(record["size_bytes"], 0),
            display_hash(record["sha256"]),
        ]
        for record in evidence.data["file_hashes"]
    ]


def build(evidence: CanonicalEvidence, output: Path, repo_root: Path) -> None:
    load_pdf_dependencies()
    claims = list(iter_claims(evidence))
    issues = list(iter_issues(evidence))
    if len(claims) > MAX_ARTIFACT_ROWS or len(issues) > MAX_ARTIFACT_ROWS:
        raise EvidenceContractError("claim or issue ledger exceeds the PDF artifact row cap")
    if len(evidence.data["file_hashes"]) > MAX_ARTIFACT_ROWS:
        raise EvidenceContractError("file hash ledger exceeds the PDF artifact row cap")
    screenshots = verified_screenshots(evidence, repo_root)
    generated_date = evidence.generated_at.date().isoformat()
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.{os.getpid()}.{uuid.uuid4().hex}.partial")
    doc = BaseDocTemplate(
        str(temporary),
        pagesize=A4,
        leftMargin=LEFT,
        rightMargin=RIGHT,
        topMargin=TOP,
        bottomMargin=BOTTOM,
        title="Voxel-Native Canonical QA Evidence Dossier",
        author="Voxel-Native evidence pipeline",
        subject="Manifest-backed visual, route, and planetary-streaming evidence",
    )
    frame = Frame(LEFT, BOTTOM, CONTENT_W, PAGE_H - TOP - BOTTOM, id="content")
    doc.addPageTemplates([PageTemplate(id="report", frames=[frame], onPage=page_chrome(generated_date))])

    story: list[Any] = [
        Spacer(1, 3 * mm),
        paragraph("CANONICAL EVIDENCE / NO INFERRED RESULTS", "kicker"),
        paragraph("Voxel-Native QA evidence dossier", "title"),
        paragraph("A bounded rendering of one explicit evidence manifest - not a scan of latest runs.", "subtitle"),
        matrix(
            ["GENERATED", "CLASSIFICATION", "RUNS", "MANIFEST SHA-256"],
            [[generated_date, evidence.data["overall_classification"], len(evidence.runs), evidence.manifest_sha256]],
            [27 * mm, 35 * mm, 17 * mm, CONTENT_W - 79 * mm],
        ),
        Spacer(1, 4 * mm),
    ]
    if screenshots:
        run, display, path, record = screenshots[0]
        story.extend(
            [
                scaled_image(path, CONTENT_W),
                paragraph(
                    f"Manifest-referenced PNG: {display} | run {run['input_path']} | sha256 {display_hash(record['sha256'])}",
                    "caption",
                ),
            ]
        )
    story.extend(
        [
            paragraph("Evidence boundary", "h1"),
            paragraph(
                "Every run below uses the current report schema, route-only frame-time quantiles, explicit viewport provenance, planetary live values and hard budgets, and manifest-referenced PNG identities. The aggregate remains Observed because runtime measurements are observations even when integrity and budget checks Passed."
            ),
            callout(
                "No fabricated release result",
                "This manifest contains no automated test-suite transcript or test total, so this PDF reports none. PNG completion and hashes prove byte identity and container completion, not perceptual visual quality.",
                PALE,
            ),
            PageBreak(),
            paragraph("Run evidence", "h1"),
            matrix(
                RUN_TABLE_HEADERS,
                run_rows(evidence),
                [width * mm for width in RUN_TABLE_WIDTHS_MM],
            ),
            *budget_flowables(evidence),
            PageBreak(),
            *claim_flowables(claims),
            paragraph("Issue ledger", "h1"),
        ]
    )
    if issues:
        story.append(
            matrix(
                ["Scope", "Class", "Code / field", "Recorded message"],
                [[scope, issue["classification"], f"{issue['code']} / {issue['field']}", issue["message"]] for scope, issue in issues],
                [31 * mm, 18 * mm, 45 * mm, CONTENT_W - 94 * mm],
            )
        )
    else:
        story.append(paragraph("The manifest records no issues for this evidence set."))
    story.extend(
        [
            PageBreak(),
            paragraph("Evidence file identity", "h1"),
            matrix(
                FILE_IDENTITY_HEADERS,
                file_identity_rows(evidence),
                [width * mm for width in FILE_IDENTITY_WIDTHS_MM],
            ),
            paragraph("Standing interpretation limits", "h1"),
            *bullets(
                [
                    "One manifest run records one viewport; it does not complete the responsive viewport and DPI matrix.",
                    "Route FPS and frame-time quantiles describe the recorded route, build, and hardware; no universal threshold or A/B uplift is inferred.",
                    "PNG hashes prove exact bytes. Human review must still inspect overlap, clipping, holes, repetition, lighting, transitions, and motion.",
                    "Hashes do not prove authorship, an unrecorded Git revision, or source correspondence outside serialized provenance.",
                    "Automated test totals require a separately hashed gate transcript and are deliberately absent here.",
                ]
            ),
        ]
    )
    doc.build(story)
    publish_no_clobber(temporary, output)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--check-only", action="store_true")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        repo_root = args.repo_root.resolve(strict=False)
        evidence = load_canonical_evidence(args.evidence_manifest)
        output = validate_output_path(args.output, repo_root, ".pdf")
        if args.check_only:
            verified_screenshots(evidence, repo_root)
        else:
            build(evidence, output, repo_root)
    except (EvidenceContractError, OSError, ValueError) as error:
        print(f"PDF artifact rejected: {error}", file=os.sys.stderr)
        return 2
    print(json.dumps(validation_summary(evidence, output), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
