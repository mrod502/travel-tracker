# Context Optimization — Repository Prompt Structure

**Date**: August 16, 2026

**Decision**: Consolidated repository context to improve agent responsiveness while maintaining quality.

## Changes Made

### 1. Deleted Redundant Files
- `IMPLEMENTATION_PROGRESS.md` — Duplicate of `.knowledge/implementation/status.md`
- `README.md` boilerplate content — Replaced with accurate project overview

### 2. Kept (Not Deleted)
- `FULLNODE_INTEGRATION_HANDOFF.md` — Contains important handoff documentation not yet reviewed

### 3. New Files Created
- `.qwen/RUST_GUIDE.md` — Rust development conventions (agent-harness agnostic location)
- `.qwen/FAST_PROMPT.md` (~100 lines) — Concise context for fast/explore subagents
- `.qwen/CONTEXT.md` — Project context summary (see below)

### 4. Consolidated `.knowledge/AGENTS.md`
- Removed "System at a Glance" section (duplicate of architecture docs)
- Removed "Critical Open Questions" section (duplicate of Blind Spots)
- Removed duplicate "Quick Navigation" section
- Removed duplicate "Document Maintenance" section
- **Result**: Reduced from ~260 lines to ~212 lines

## Context Loading Strategy

### Full Context (Main Agent)
- `.qwen/FAST_PROMPT.md` — System overview and current status
- `.qwen/RUST_GUIDE.md` — Rust conventions (when relevant)
- `.knowledge/AGENTS.md` — Knowledge base index with blind spots
- Relevant skill files (automatically loaded by keyword triggers)
- Project memory (`MEMORY.md`)

### Fast/Explore Agent Context
- `.qwen/FAST_PROMPT.md` only (~100 lines)
- Skills loaded on-demand by keyword
- Codebase search tools for discovery
- Full `.knowledge/` docs only when explicitly needed

## Benefits

1. **Faster subagent startup** — Fast agents load ~100 lines instead of ~500+
2. **Reduced duplication** — Single source of truth for each topic
3. **Better organization** — Context files in `.qwen/` separate from knowledge docs
4. **Maintainable** — Clear boundaries between quick reference and deep documentation

## File Locations Summary

| Purpose | Location |
|---------|----------|
| Project README | `/workspace/README.md` — User-facing project overview |
| Rust conventions | `.qwen/RUST_GUIDE.md` — Language-specific best practices |
| Fast agent context | `.qwen/FAST_PROMPT.md` — Concise system overview for subagents |
| Knowledge base | `.knowledge/AGENTS.md` — Comprehensive documentation index |
| Skills | `.qwen/skills/` — Domain-specific expertise (CBOR, H3, provenance) |
