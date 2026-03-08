# Side Session: Vision-Augmented Figure Extraction for research-index

## Context

research-index (~/dev/research-index) is a hybrid semantic search MCP server for research papers. It ingests PDFs via PyMuPDF (text extraction) and indexes chunks with embeddings (nomic-embed-text via Ollama).

**Problem**: Figures, diagrams, and image-based tables are lost during text-only extraction. For research papers, this means losing 30-50% of critical information (architecture diagrams, comparison charts, flow visualizations).

**GitHub issue**: https://github.com/dutiona/research-index/issues/20

## What exists

- PyMuPDF is already a dependency (used for PDF text extraction)
- Vision models available on Ollama (Windows host, accessible from WSL2): gemma3:27b, qwen3-vl:8b, qwen3-vl:30b
- Configurable LLM client added in PR #14 (provider + base_url + model, supports Ollama native + OpenAI-compat)
- Vision tools deployed at ~/.local/opt/vision-tools/
- Rendering: `fitz.open(pdf) -> page.get_pixmap(matrix=fitz.Matrix(2,2)).save('out.png')` works

## What to build

A new extraction pass that:

1. Iterates PDF pages, detects figures (via PyMuPDF image detection or full-page render)
2. Renders each figure region to PNG
3. Sends PNG to a vision model with a structured prompt
4. Stores the vision model's description as a searchable chunk linked to the paper

### Design decisions to make

- Separate tool (`extract_figures_tool(paper_id)`) vs integrated into `ingest()`?
- Store PNGs on disk or just descriptions?
- Schema: extend chunks table with `source_type='figure'` or new `figures` table?
- How to detect figure boundaries vs just rendering full pages?
- Prompt engineering for the vision model (structured JSON output?)

### Key constraints

- Must not break existing ingestion pipeline
- Must handle the SQLite threading issue (#19) -- don't share connections across threads
- Vision model runs on remote Ollama (Windows host) -- need to handle base64 image encoding for the API
- ETA gate like extract_structure (vision calls are ~3-5s each)

## Relevant files

- `src/research_index/server.py` -- MCP tool definitions
- `src/research_index/extraction.py` -- map-reduce extraction pipeline (reference for pattern)
- `src/research_index/db.py` -- schema, get_connection
- `docs/plans/2026-03-07-map-reduce-extraction-design.md` -- design doc pattern to follow

## Test with

CoALA paper: ~/dev/autonomous-agent-research/papers/coala-2309.02427.pdf

- Page 4, Figure 2: Soar architecture (two-part: A=memory hierarchy, B=decision cycle)
- Page 7, Figure 3: CoALA framework diagram (the main contribution)

## Workflow

Use /super-plan or brainstorming skill to design first. Write a design doc to docs/plans/. Then implement with TDD. File a PR when done.
