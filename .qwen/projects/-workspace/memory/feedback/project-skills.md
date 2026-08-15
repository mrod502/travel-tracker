---
name: project-custom-skills
description: Custom skills for provenance, H3 geospatial, and CBOR encoding
type: feedback
---

This project has three custom skills in `.qwen/skills/` that should be automatically invoked for relevant tasks:

**`canonical-cbor`** — For CBOR serialization, signing occurrence data, signature verification. Triggers on: CBOR, signing, provenance, canonical encoding.

**`h3-geospatial-indexing`** — For H3 geospatial indexing, spatial queries, location data. Triggers on: H3, geospatial, location, spatial query, hex grid.

**`provenance-verification`** — For node identity, occurrence authenticity, signature verification. Triggers on: provenance, node identity, origin_node_id, CA credential.

**Why:** These skills contain critical domain knowledge about cryptographic signing, geospatial indexing, and decentralized provenance that must be applied consistently. They are referenced in AGENTS.md under "Custom Project Skills" to ensure automatic discovery.

**How to apply:** When tasks involve occurrence signing, location storage, or node verification, automatically consider and invoke the relevant skill using `/skill <name>`.
