#!/usr/bin/env python3
"""Compile explicitly named candidate JSON files into one canonical Evidence Graph."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Sequence

from evidence_model import (
    EvidenceGraphError,
    _path_comparison_key,
    compile_candidate_files,
    validate_output_path,
    write_graph,
)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--candidate",
        action="append",
        required=True,
        metavar="FILE.json",
        help="explicit candidate JSON file; repeat for multiple files",
    )
    parser.add_argument(
        "--output",
        required=True,
        metavar="GRAPH.json",
        help="explicit graph output outside saves/qa_runs/agent_runs",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    repo_root = Path(__file__).resolve().parents[2]
    try:
        destination = validate_output_path(args.output, repo_root=repo_root)
        destination_key = _path_comparison_key(destination)
        for candidate_path in args.candidate:
            try:
                candidate_key = _path_comparison_key(
                    Path(candidate_path).resolve(strict=True)
                )
            except (OSError, RuntimeError) as error:
                raise EvidenceGraphError(
                    f"candidate path cannot be resolved: {candidate_path}: {error}"
                ) from error
            if candidate_key == destination_key:
                raise EvidenceGraphError("output path must not overwrite an explicit candidate input")
        graph = compile_candidate_files(args.candidate, repo_root=repo_root)
        write_graph(graph, destination, repo_root=repo_root)
    except (EvidenceGraphError, OSError) as error:
        print(f"evidence graph rejected: {error}", file=sys.stderr)
        return 2
    print(
        f"wrote {destination} with {graph['summary']['node_count']} node(s), "
        f"{graph['summary']['edge_count']} edge(s), sha256={graph['graph_sha256']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
