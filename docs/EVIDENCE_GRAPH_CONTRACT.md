# Voxel-Native Evidence Graph Contract

Status: Evidence Graph schema `1.0.0`; Artifact View schema `1.0.0`

Compiler: `tools/evidence/build_evidence_graph.py`

Schemas:

- `tools/evidence/schema/evidence-graph-1.0.0.schema.json`
- `tools/evidence/schema/artifact-view-1.0.0.schema.json`

## Purpose and authority boundary

The Evidence Graph is the normalized, machine-readable truth boundary between
explicit evidence/workflow inputs and every downstream projection: documents,
PDFs, spreadsheets, presentations, Sites, visualizations, GitHub summaries, and
future renderer adapters. It does not replace the QA Evidence Manifest, invent
missing measurements, certify a release, or turn presentation layout into
domain truth.

The graph compiler is deliberately dependency-free. It performs bounded JSON
parsing, normalization, identity derivation, relationship validation, task-DAG
validation, canonical hashing, and atomic output. Renderer code should consume
an explicit graph or a validated Artifact View and remain layout-only.

## Design decision and alternatives

The success metric for Phase 1 is byte-identical output and node identities for
identical explicit input bytes supplied from the same canonical paths,
independent of CLI input order, while every unit of work and memory remains
under a documented fixed cap. Canonical paths are serialized provenance, so
moving the same bytes to another path intentionally changes the graph. Node and
edge records are normalized independently of their array order. Reordering
records inside a Candidate changes that source file's byte hash and therefore
the full provenance-bound graph hash by design, while derived node IDs and
normalized semantic node/edge arrays remain stable.

Three approaches were evaluated:

1. Renderer-specific tables were rejected. They repeat selection,
   classification, and aggregation logic in every artifact builder and permit
   DOCX, PDF, workbook, deck, and site claims to drift.
2. A free-form property graph was rejected. Arbitrary node kinds, relations,
   and caller-selected IDs are flexible but make validation and compatibility
   proofs ambiguous.
3. A typed normalized graph plus a smaller Artifact View was selected. The
   compiler owns semantics once; renderers receive stable, traceable projection
   rows without reinterpreting evidence.

This is a correctness and reproducibility improvement, not a measured runtime
performance claim. Phase 1 has dependency-free determinism and cap tests; it
does not yet include a production-size benchmark or migrate existing builders.

## Explicit input contract

Every compiler input is an explicitly named JSON file:

```powershell
python -B tools/evidence/build_evidence_graph.py `
  --candidate output/evidence/cohort-a.json `
  --candidate output/evidence/cohort-b.json `
  --output output/evidence/evidence-graph.json
```

The compiler:

- accepts 1 through 64 explicit `.json` files;
- rejects parent traversal, any path component named `latest`, canonical input
  duplicates, missing files, directories, and unsupported extensions;
- never scans a directory, the repository, `qa_runs`, or “newest” state;
- hashes the exact bytes of every input and records canonical display paths;
- captures inputs in fixed 1 MiB chunks with a one-byte overflow sentinel, so
  a concurrently growing file cannot bypass the shared 16 MiB memory budget;
- rejects duplicate JSON object keys, malformed UTF-8/JSON, unsupported
  constants, non-finite numbers, excessive nesting, and values outside fixed
  limits;
- requires an explicit `.json` output and rejects output under repository
  `saves/`, `qa_runs/`, or `agent_runs/`;
- rejects an output that resolves to any explicit candidate input;
- writes through a same-directory temporary file and atomic replacement.

Candidate files use this strict compiler-input shape:

```json
{
  "schema_version": "evidence-graph-candidate/1.0.0",
  "nodes": [
    {
      "alias": "qa-natural",
      "kind": "qa_run",
      "identity": {"report_sha256": "...", "run_label": "natural"},
      "title": "Natural QA run",
      "classification": "Observed",
      "description": "Optional bounded prose",
      "data": {"manifest_path": "output/evidence/manifest.json"}
    },
    {
      "alias": "source-manifest",
      "kind": "source_file",
      "identity": {"sha256": "...", "logical_path": "output/evidence/manifest.json"},
      "title": "Evidence manifest",
      "classification": "Observed"
    }
  ],
  "edges": [
    {
      "relation": "derived_from",
      "from": "qa-natural",
      "to": "source-manifest",
      "data": {}
    }
  ]
}
```

`alias`, `kind`, `identity`, and `title` are required on every node.
`classification`, `task_state`, `description`, and `data` are syntactically
optional, but `task_state` is required (and `classification` forbidden) for a
`task`; `classification` is required for `claim` and `issue`, optional only for
the supported evidence kinds, and forbidden elsewhere. Only `data` is optional
on an edge. Unknown fields are rejected. `alias` is a bounded compiler-local
reference and is never copied into the authoritative graph.

## Authoritative node identity

Every authoritative node ID has exactly this form:

```text
<kind>:<sha256(canonical-json(identity))>
```

Canonical identity JSON is UTF-8 with keys sorted, no insignificant
whitespace, no ASCII escaping, no `NaN`/infinity, and deterministic JSON scalar
spelling. The node kind is an identity namespace: two identical identity
objects with different kinds have different IDs. Callers cannot provide an
authoritative ID. Duplicate derived identities are rejected rather than
silently merged, because silent merging could hide disagreement between
explicit inputs.

Identity objects answer “which durable thing is this?” Mutable observations,
labels, descriptions, status, and renderer hints belong in other fields. A
change to identity intentionally changes the ID; input ordering does not.

## Node kinds

The only legal node kinds are:

| Kind | Purpose |
|---|---|
| `source_file` | Hashed source, transcript, report, screenshot, or manifest file. |
| `qa_run` | One explicit QA execution identity. |
| `observation` | A measured or directly inspected value. |
| `claim` | A narrow statement with explicit evidence classification. |
| `issue` | A bounded defect, contradiction, absence, or risk statement. |
| `gate_run` | A test, lint, release-gate, or validation execution. |
| `visual_review` | Human/agent visual inspection with bounded route/surface scope. |
| `task` | Workflow work item with task state, never evidence classification. |
| `agent` | Agent identity/capability record. |
| `artifact` | Deliverable or renderer target identity. |
| `external_asset` | Explicit external reference or asset, never an implicit web scan. |
| `github_check` | GitHub status/check identity and observation. |

## Evidence classifications and task states

Evidence classification and workflow state are separate dimensions. They must
never be translated into each other.

The five Evidence Manifest classifications are preserved exactly:

- `Passed`
- `Observed`
- `Rejected`
- `Planned`
- `Blocked`

`claim` and `issue` nodes require a classification. Other evidence-bearing
node kinds may retain one when supplied. `task`, `agent`, and `artifact` nodes
must not carry an evidence classification.

Only `task` nodes carry a task state, exactly one of:

- `planned`
- `ready`
- `running`
- `blocked`
- `review`
- `complete`
- `cancelled`

The spelling collision between Evidence `Blocked` and task `blocked` is not a
mapping. For example, a completed task may have produced a `Rejected` visual
review, and a `Blocked` legacy evidence claim may be documented by a `complete`
analysis task.

## Relations and topology

The only legal relations are:

- `derived_from`
- `supports`
- `contradicts`
- `generated_by`
- `validated_by`
- `assigned_to`
- `depends_on`
- `blocks`
- `renders`
- `published_as`
- `references`

Every endpoint must exist. Duplicate `(relation, from, to)` triples are
rejected even when their `data` differs, because parallel triples make relation
cardinality and status ambiguous. `depends_on` must connect `task -> task`, may
not point to itself, and the complete task dependency subgraph must be acyclic.
`assigned_to` must connect `task -> agent`. Other relations deliberately retain
broader endpoint compatibility in `1.0.0`; their meaning is explicit in the
relation name and their endpoint records, without prematurely freezing a
domain mapping that existing artifact builders do not yet emit.

## Fixed limits

All limits are fail-closed architectural contracts:

| Resource | Maximum |
|---|---:|
| Candidate inputs | 64 |
| Candidate bytes, all inputs combined | 16 MiB |
| Serialized graph | 16 MiB |
| Nodes | 12,000 |
| Edges | 32,000 |
| Task nodes | 512 |
| Agent nodes | 48 |
| `depends_on` edges | 2,048 |
| External asset nodes | 32 |
| Artifact nodes | 32 |
| Authoritative ID or candidate alias | 256 characters |
| Path | 4,096 characters |
| Individual string | 16,384 characters |
| JSON nesting depth | 64 |
| Portable integer interval | signed 64-bit |

Counts are validated before normalization where possible and again on the
canonical graph. The output writer also checks the pretty-serialized byte
size. No count is inferred from a renderer or converted to zero when absent.

## Canonical order, hash, and bytes

The normalized graph stores:

```text
schema_version
generator
evidence_classifications
task_states
source_inputs[]
nodes[]
edges[]
summary
graph_sha256
```

Inputs are sorted by canonical path. Nodes are sorted by `(kind, id)`. Edges
are sorted by `(relation, from, to, canonical data hash)`. Summary maps emit
every legal enum value, including zero counts, so consumers do not need to
invent missing categories.

`graph_sha256` is SHA-256 over compact canonical JSON of the complete graph
payload excluding `graph_sha256`. The saved file is pretty JSON with sorted
keys, UTF-8, LF newlines, strict finite numbers, and one final newline. The
canonical payload hash and the presentation serialization are intentionally
separate; both are deterministic.

Timestamps and filesystem modification times are omitted. Source input paths,
byte hashes, and sizes are retained, so the graph proves exactly which explicit
bytes were compiled. A hash proves byte identity, not correctness, authorship,
visual quality, or release readiness.

## Artifact View contract

An Artifact View is a bounded, renderer-facing projection. Phase 1 validates
this schema but does not generate views or artifacts. A view is bound to one
`source_graph_sha256` and one `artifact` node, with a canonical view ID and
view hash. Its `sections -> rows -> cells` structure carries non-empty,
canonically sorted `source_node_ids` at every projection level. Row sources
must be represented by their section and cell sources by their row. Section IDs
are globally unique in a view; row IDs are unique within their section. When a
source graph is supplied to the validator, its hash, artifact node, and every
projected source node must resolve exactly in that graph. A projected Evidence
classification or task state must also equal the status of every referenced
source node that carries that dimension, with at least one such source; a View
cannot relabel `Rejected` as `Passed` or task `running` as `complete`.

Rows and cells may carry either an evidence `classification` or a `task_state`,
never both. This prevents a visual component from collapsing the two semantic
dimensions. Legal view kinds are `document`, `pdf`, `spreadsheet`,
`presentation`, `site`, `visualization`, and `github`.

The Artifact View does not authorize a renderer to:

- alter values from source nodes;
- convert missing/blocked/rejected evidence to zero;
- manufacture FPS, test totals, visual verdicts, task completion, or links;
- drop source-node identity from a displayed claim;
- average incompatible observations;
- treat an attractive layout as evidence acceptance.

## Failure behavior

Compilation is all-or-nothing. Any unsupported version, key, node kind,
relation, type, non-finite value, cap overflow, duplicate alias/identity/edge,
missing endpoint, self-dependency, task cycle, unsafe path, read race, or hash
contradiction raises `EvidenceGraphError`. The CLI exits with code `2`, writes
no graph, and prints the bounded rejection reason to standard error.

There is no permissive legacy mode. A new producer must first be adapted to the
candidate contract or an explicit future schema version.

## Verification

Run the dependency-free suite:

```powershell
python -B -m unittest discover -s tools/evidence/tests -p "test_*.py" -v
```

The tests cover canonical ID namespaces, deterministic bytes and input/record
order, classification/task-state separation, exact enum/schema contracts,
duplicate aliases/identities/edges, missing endpoints, typed edge constraints,
self-dependency and multi-node task cycles, unsupported fields/versions/kinds/
relations, duplicate JSON keys, non-finite numbers, hard population and byte
caps, protected output paths, atomic CLI output, hash/summary tampering, and
Artifact View source/status integrity. All files created by tests live beneath
per-test temporary directories.

## Phase 1 integration limits

- Existing Evidence Manifest builders and consumers are unchanged.
- Existing DOCX, PDF, workbook, presentation, site, and GitHub builders do not
  consume this graph yet.
- There is no manifest-to-candidate adapter or projection compiler yet.
- JSON Schema files document structural interoperability; Python validation is
  authoritative for canonical hashes, sorted order, aggregate consistency,
  endpoint typing, task-cycle detection, shared byte caps, and path safety.
- There is no production-size throughput benchmark yet.
- Generated graph and view outputs are reproducible artifacts and are not
  committed.

These are deliberate rollback boundaries: removing `tools/evidence/` and this
contract restores the previous artifact workflow without modifying its inputs,
builders, or outputs.
