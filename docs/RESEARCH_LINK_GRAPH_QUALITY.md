# Research Link Graph Quality Report

Date: 2026-08-09

Purpose: decide whether the user's Wikipedia parent/subtopic corpus is safe to
use for Voxel-Native research routing. It is not used as technical proof.

## Dataset and grain

The intended grain is one directed relation per CSV row:

```text
parent article -> linked/discovered subtopic article
```

Inputs:

| Input | Size | SHA-256 | Role |
|---|---:|---|---|
| `cf64.../pasted-text.txt` | 1,074,970 bytes | `FEBFFD2182C7D53D0595EAA1BE0B7C6C25A23014E5C90C4E6FB6983B18668169` | canonical four-column relationship CSV |
| `0c7a.../pasted-text.txt` | 263,378 bytes | `1EE63DBE42592E9625B3EE158476DA2F62CF27A6FA326B80D144B4AAB2B29773` | child/subtopic URL list |
| `11ad.../pasted-text.txt` | 265,912 bytes | `7B53CA350C7ADBD600BD8A86B214E5E245A3F9862D86F818C3CD4DAF79661FDC` | supplementary URL list containing most parents plus most children |

The reusable, read-only audit is
`scripts/audit-research-links.ps1`. It does not edit the source corpus.

## Checks performed

- required CSV columns and row completeness;
- URL scheme, host and `/wiki/` shape;
- directed-pair duplicates and self-loops;
- parent and child cardinality;
- case-sensitive and case-folded URL uniqueness;
- coverage of graph parents/children by each raw URL list;
- parent- and child-degree distributions;
- universal-hub detection;
- Wikipedia meta/discussion/user/portal seed share;
- manual source-page checks for selected high-degree edges.

## Compact profile

| Metric | Result |
|---|---:|
| CSV rows | 8,129 |
| complete rows | 8,129 (100%) |
| invalid/non-German-Wikipedia URL rows | 0 |
| self-loops | 0 |
| unique parents | 118 |
| case-folded unique subtopics | 5,527 |
| case-folded unique directed pairs | 8,125 |
| duplicate directed-pair rows | 4 |
| median links per parent | 49 |
| parent-degree p95 / max | 245 / 470 |
| median child parent-count | 1 |
| child-degree p95 / max | 3 / 117 |
| meta/discussion/user/portal parents | 17/118 (14.41%) |
| rows from those meta parents | 634/8,129 (7.80%) |

The canonical child list has 5,534 exact strings but 5,527 case-folded URLs.
The seven-row difference is URL spelling/case variation, not seven additional
semantic research topics.

The supplementary list has 5,577 exact strings and 5,570 case-folded URLs. It
covers 5,510/5,527 graph children and 117/118 parents. It is therefore useful
as a supplement, but it is not a strict or complete master list.

## Findings

### High: graph degree is not semantic importance

`Voxel` appears as a child of 117 of 118 parents (99.15%); the only parent
without that edge is `Voxel` itself. This is a likely extraction/root-link
artifact mixed with some genuine links. A source-page spot check found a real
voxel reference on `Ahorne` (a micro-CT image), but no `Voxel` text on
`Blut-Hirn-Schranke`. The universal edge must not be treated as evidence that
every parent contributes equally to voxel-engine design.

Impact: a naive degree rank would make the extraction seed look like the most
important discovery and would bury smaller but stronger bridges such as
topological voxelization, virtual texturing, watershed flow, surface splatting,
or plant-development grammars.

Remediation: ignore universal hubs for relevance ranking; use them only for
navigation. Validate every selected technical bridge against its source page
and then a primary/official source.

Confidence: high.

### High: article fan-out strongly biases the corpus

Parent degree ranges from 1 to 470. Large botany and medical articles dominate
raw volume (`Blut-Hirn-Schranke` 470, `Ahorne` 455, `Muscheln` 313,
`Blatt (Pflanze)` 290), while a concise algorithm page may expose far fewer
links. This measures page breadth, templates and editorial style, not transfer
value to the engine.

Impact: studying all 5,527 pages uniformly would spend most effort on taxonomy,
publishers, dates, institutions and medical detail that does not affect the
product.

Remediation: route by engine question first, then inspect a bounded relevant
neighborhood. Preserve the raw graph so a future question can reopen a route.

Confidence: high.

### Medium: 17 seed parents are non-article/meta material

User drafts, talk archives, portals, quality-assurance discussions and deletion
discussions account for 14.41% of seed parents and 7.80% of relation rows. They
can reveal vocabulary or historical disputes, but cannot support a technical
decision.

Impact: unsupported claims and dead-end catalog links may look equivalent to
reviewed article references.

Remediation: retain these rows in the source graph, mark them
`discovery-only`, and exclude them from primary evidence and automatic ranking.

Confidence: high.

### Medium: URL identity requires canonicalization

Both raw lists have seven pairs that are distinct under exact string comparison
but merge under case-folding. MediaWiki title and percent-encoding behavior is
more nuanced than a generic lowercase operation, so normalization must preserve
the original URL while maintaining a canonical comparison key.

Impact: agents could report inconsistent topic totals or study the same page
twice.

Remediation: keep `original_url` plus `comparison_key`; resolve redirects only
for shortlisted pages, not by mutating the raw corpus.

Confidence: high for the count, medium for semantic identity until redirects
are resolved.

### Low: four duplicate relation rows

Duplicate parent/subtopic pairs are:

- `7 Days to Die -> Heise online`;
- `Blut-Hirn-Schranke -> Tight Junction`;
- `Second Reality -> Public Domain`;
- `Wikipedia:Löschkandidaten/25. August 2020 -> Simple system`.

Impact: negligible for manual study, but they bias counts in automated ranking.

Remediation: deduplicate on canonical `(parent_url, subtopic_url)` for analysis,
while retaining raw row counts in the audit.

Confidence: high.

### Positive: structural completeness is strong

Every CSV row has all four required fields, all URLs match HTTPS German
Wikipedia article shape, and there are no self-loops. The exact child list
covers all 5,527 case-folded graph children.

Impact: the graph is reliable as a discovery index after the ranking and
normalization caveats above.

Confidence: high.

## Safe downstream use

The graph is approved for:

- concept discovery and terminology expansion;
- finding cross-domain candidate routes;
- building a human-reviewed research queue;
- preserving provenance from parent topic to discovered subtopic.

It is not approved for:

- ranking technical importance directly by link count;
- treating every relation as a verified hyperlink or causal connection;
- citing Wikipedia relationships as proof of an algorithm;
- automatically implementing every discovered concept;
- medical, physical, mathematical or performance claims without primary
  evidence.

## Automated guardrails

Future corpus drops should fail the audit when:

- required columns are missing;
- any row is incomplete;
- a URL leaves the expected HTTPS German Wikipedia article namespace;
- a self-loop appears unexpectedly;
- graph-child coverage by the canonical list falls below 100%;
- counts change without a recorded corpus version/hash.

Universal hubs, duplicate rates, meta-parent share and degree distributions
should be warnings rather than hard failures because the underlying graph can
legitimately evolve.
