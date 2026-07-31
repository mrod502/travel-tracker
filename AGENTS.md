# Agent Instructions

**Setup Checklist**:
1. Copy the relevant language guide from `languages/` into this file
2. Delete the `languages/` directory to avoid context pollution
3. Delete this `SETUP.md` file after completion
4. Review `.qwen/settings.json` for model configuration

**Note**: This file should be populated with project-specific documentation after setup. During the initial project setup, copy the relevant language-specific guide from `languages/` and remove guides for languages not used in this project to avoid polluting the agent's context.

---

## Project Overview

This is a [PROJECT_TYPE] application for [PROJECT_DESCRIPTION].

## Key Components and Architecture

### Core Directory Structure
- **src/** - Main source code directory
- **cmd/** - Command-line interface (if applicable)
- **api/** - API layer (if applicable)
- **services/** - Service implementations
- **types/** - Shared data types
- **utils/** or **helpers/** - Utility functions
- **tests/** or **test/** - Test files

### Key Features

1. **[Feature 1]**: [Description]
2. **[Feature 2]**: [Description]
3. **[Feature 3]**: [Description]

### Key Patterns Used

1. **[Pattern 1]**: [Description]
2. **[Pattern 2]**: [Description]
3. **[Pattern 3]**: [Description]

### Data Flow

1. [Step 1 - Entry point]
2. [Step 2 - Processing]
3. [Step 3 - Output/Storage]

### External Dependencies

[List key dependencies with brief descriptions]

- [Dependency 1] - [Purpose]
- [Dependency 2] - [Purpose]
- [Dependency 3] - [Purpose]

---

## Development Guidelines

### Code Style

- Follow [LANGUAGE] idioms and conventions
- Use meaningful variable and function names
- Write comprehensive tests for new features
- Document public APIs and complex logic

### Testing

- Run tests with: `[TEST_COMMAND]`
- Maintain test coverage above [THRESHOLD]%
- Tests should be isolated and deterministic

### Build & Run

- Build: `[BUILD_COMMAND]`
- Run: `[RUN_COMMAND]`
- Test: `[TEST_COMMAND]`

---

## Build and Cache Directory Policy

### Language-Agnostic Guidelines

- **Never commit build artifacts or cache files to Git** — this is the golden rule
- Follow language-specific best practices for where builds and caches live
- Ensure all generated files are properly excluded via `.gitignore`
- Configure tools to use appropriate cache directories (system temp or language defaults)

### Temporary File Locations

1. **Inside Docker (LLM container):**
   - Use `/tmp` — mounted as tmpfs, automatically cleaned on container restart
   - Use `[LANGUAGE_SPECIFIC_CACHE_DIRS]` for language-specific caches
   - Use `/home/dev` — named volume for user home persistence
   - Some languages may write to project folders (e.g., `target/` in Rust, `node_modules/` in Node.js) — this is fine as long as they're ignored

2. **On host machine:**
   - Use system `/tmp` (or `$TMPDIR`) for temporary files
   - Use `~/.cache` for application cache files
   - Project-local build directories (e.g., `build/`, `dist/`, `target/`) are acceptable if ignored

### Language-Specific Examples

- **Go**: Prefer `/go/build-cache`, `/go/pkg/mod`, `/go/bin` (volumes) or `GOCACHE` env var
- **Rust**: `target/` directory in project root is standard — ensure it's in `.gitignore`
- **Node.js**: `node_modules/` in project root is standard — ensure it's in `.gitignore`; cache in `~/.npm`
- **Python**: Virtual environments (`.venv/`, `venv/`) in project root — ensure in `.gitignore`; pip cache in `~/.cache/pip`
- **Java/Maven**: `target/` directory — ensure in `.gitignore`; Maven repo in `~/.m2`
- **TypeScript**: `dist/`, `build/`, `.tscache/` — ensure in `.gitignore`

### If a Tool Defaults to Workspace Directories

Some build systems and package managers default to creating directories in the project root:

1. **Check `.gitignore` first** — add the directory pattern if missing
2. **Configure environment variables** to redirect caches (e.g., `npm_config_cache`, `CARGO_HOME`)
3. **Override tool configuration** if the language allows (e.g., `.npmrc`, `cargo.toml`)
4. **Report issues** if you cannot configure the tool appropriately

**Remember**: The goal is clean Git history, not preventing all project-local directories.
