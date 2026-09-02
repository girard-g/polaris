# Product Strategy — Free vs Pro

## Positioning

Polaris is a local-first RAG server that gives coding agents fast, ranked answers
from project documentation over MCP. It is agent-agnostic (Claude Code, Cursor,
Codex, any MCP client) and runs as a single static binary with no cloud
dependency.

### Competitive landscape

| Tool | Focus | Overlap with Polaris |
|---|---|---|
| Cursor indexing | Code search, IDE-locked | Different domain (code vs docs) |
| Serena | Code structure via LSP/AST | Complementary, not competing |
| GrepAI / grep.app | Semantic code search, cloud | Different domain, different model |
| Context7 | Framework docs via API | Curated doc library, cloud-dependent |

Polaris's niche: **documentation search for any MCP-compatible agent, local-first,
private, project-specific.** The docs that matter most — internal architecture
decisions, team runbooks, onboarding guides — are exactly the ones not in the
LLM's training data and not on Context7.

---

## Free / Pro split

**Principle:** The free core must be genuinely excellent at search. Pro sells
itself through the problems that emerge once you rely on Polaris daily.

- Free = best-in-class retrieval for individual developers
- Pro = intelligence, breadth, and teams

### Retrieval pipeline

| Feature | Free | Pro |
|---|---|---|
| Hybrid KNN + BM25 + RRF + MMR | x | x |
| Cross-encoder reranking (Jina Turbo, opt-in) | x | x |
| Additional embedding model options | x | x |
| Smarter chunking (structural signals, Jaccard grouping) | x | x |
| ColBERT late-interaction reranking | | x |
| Query expansion / rewriting | | x |
| Hypothetical Document Embeddings (HyDE) | | x |

### Intelligence / analytics

| Feature | Free | Pro |
|---|---|---|
| Token savings (`polaris savings`) | x | x |
| Coverage gaps — surface queries that match nothing in the docs | | x |
| Doc quality feedback — identify chunks that never match any query | | x |
| Retrieval quality dashboard (precision/recall trends) | | x |

### Format and ingestion

| Feature | Free | Pro |
|---|---|---|
| Markdown (.md) | x | x |
| Non-markdown (txt, rst, code, PDF) via polaris-ingest | | x |

### Scale and teams

| Feature | Free | Pro |
|---|---|---|
| Single-user local MCP server | x | x |
| Multi-DB search (BankSet) | x | x |
| Multi-tenant (mTLS, namespaces) | | x |
| Web UI (admin + search) | | x |
| Remote indexing (CI/CD push) | | x |
| User enrollment + cert management | | x |
| Docker / service deployment | | x |

### Agent integration

| Feature | Free | Pro |
|---|---|---|
| MCP server (search, index, status) | x | x |
| PostToolUse auto-index hook | x | x |
| UserPromptSubmit auto-search hook | x | x |
| Multi-agent shared context | | x |

---

## Rationale

**Free makes the retrieval pipeline untouchable.** Cross-encoder reranking,
better chunking, more embedding models — all free. This is what makes someone
choose Polaris over grepping. A strong open-source core drives adoption and
community credibility.

**Pro sells when usage reveals gaps.** After a week of daily use, patterns
emerge: agents keep searching for topics your docs don't cover (coverage gaps),
chunks that never match anything indicate stale or poorly written docs (doc
quality feedback). These insights only have value after sustained use of the
free tier — they are the natural upsell.

**Pro sells breadth.** You love Polaris for markdown, now you want it for PDF
runbooks and RST API docs (polaris-ingest). Your team wants shared access
(multi-tenant). Your CI pipeline should push fresh docs on every merge (remote
indexing).

**The conversion funnel:** Individual developer tries free tier, can't work
without it, brings it to the team, team needs pro features.

---

## Directions to explore (pro)

### Coverage gaps

Track queries over time. Surface patterns like: "agents searched for
'deployment' 12 times this week but the top result scored below 0.3 every time."
Actionable signal that the docs need a deployment section.

Implementation: extend `search_log` with score tracking, add a periodic
analysis pass or CLI command (`polaris insights`).

### Doc quality feedback

Identify indexed chunks that have never (or rarely) been returned as a top-k
result. These are candidates for rewriting, merging, or removal. Flip side:
identify chunks that are returned often but with low scores — they match
queries but don't satisfy them.

Implementation: join `search_log` against chunk IDs in results, aggregate
over a time window.

### Multi-agent shared context

When multiple agents (or multiple sessions of the same agent) work on a
project simultaneously, they duplicate search work. A shared context layer
could deduplicate and cache recent search results, or provide a "what has
the team been looking at" view.

This is speculative and depends on how multi-agent MCP workflows evolve.
