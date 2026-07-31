# LLM Project Template

This is a language-agnostic project template for setting up repositories to be developed with LLM coding agent assistance (e.g., Qwen Code, Claude Code).

## What's Included

- **AGENTS.md** - Agent instructions and project documentation (template)
- **Dockerfile.dev** - Isolated development container for LLM agents
- **compose.yaml** - Docker Compose configuration for LLM development environment
- **.gitignore** - Git ignore patterns with language-agnostic defaults
- **SETUP.md** - Step-by-step guide to customize for your language and project
- **languages/** - Language-specific setup guides (Go, Node.js, Rust, Python, C/C++, React Native)
- **.qwen/** - Qwen Code configuration (settings.json, skills/)

## Quick Start

1. **Read and follow SETUP.md** - This guide walks you through:
   - Customizing Dockerfile.dev for your language/toolchain
   - Configuring compose.yaml for your services
   - Setting up language-specific caches and dependencies
   - Tailoring .gitignore for your build artifacts
   - Adapting CLAUDE.md for your project documentation

2. **Start the LLM development environment:**
   ```bash
   docker compose up -d llm
   docker compose exec llm zsh
   qwen  # or your chosen LLM agent
   ```

## Key Features

### Qwen Code Configuration

The `.qwen/` directory contains pre-configured settings for Qwen Code:

- **settings.json** - Configured for local llama.cpp server at `http://192.168.50.115:8189/v1`
- **Models configured**:
  - `Qwen3.5-122b-a10b` - Default context (~200k tokens)
  - `Qwen3.6-35b-a3b` - Fast context (~200k tokens)
- **Skills directory** - Pre-configured debugging skill and documentation for creating custom skills
- **No API key validation** - Configured for local development without authentication

### Isolated LLM Development Environment

- Non-root user with matched UID/GID for file permission compatibility
- Read-only root filesystem for security
- Named volumes for persistent caches and LLM agent state
- tmpfs for temporary scratch space
- Network isolation with dedicated Docker networks

### Language-Agnostic Design

- Supports any programming language
- Flexible cache directory configuration
- Project-local build directories allowed (when appropriate)
- Emphasis on `.gitignore` over restrictive policies

### Security Hardening

- Droops all capabilities by default
- Limits new privileges
- Constrains process counts and memory usage
- CPU resource allocation

## Customization Checklist

Before using this template in a new project:

- [ ] Replace all `[PLACEHOLDERS]` with language-specific values
- [ ] Update `.gitignore` for your language's build artifacts
- [ ] Configure Docker volumes based on your language's cache strategy
- [ ] Tailor `CLAUDE.md` to document your project's architecture
- [ ] Test that `git status` shows no unexpected build files
- [ ] Verify LLM agent can run and access code intelligence tools

## Philosophy

This template follows these principles:

1. **Clean Git history is paramount** - Never commit build artifacts or dependencies
2. **Follow language conventions** - Don't fight your language's tooling
3. **Isolation over integration** - LLM development happens in containers
4. **Persistence where it matters** - LLM state and language caches survive restarts
5. **Security by default** - Minimal privileges, read-only filesystems

## License

This template is provided as-is for your projects. Feel free to modify and adapt.
