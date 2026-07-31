# LLM Development Environment Setup

This guide walks through bootstrapping the language-specific and project-specific aspects of the LLM development framework.

## Prerequisites

- Docker Desktop or Podman installed
- Docker Compose v2.x
- Git configured with your identity
- SSH keys set up for private repository access (if applicable)

## Quick Start

1. **Copy template files to project root:**
   ```bash
   cp -r llm-project-template/* .
   rm -rf llm-project-template
   ```

2. **Create `.env` file** from template:
   ```bash
   cp .env.example .env
   ```

3. **Review and customize Qwen configuration:**
   - `.qwen/settings.json` is pre-configured for your local llama.cpp server
   - Adjust model names or endpoints if needed
   - Add custom skills to `.qwen/skills/` as needed

4. **Select your language setup:**
   - Choose the appropriate guide from `languages/` (e.g., `languages/go.md`)
   - Copy its contents into `AGENTS.md`
   - **Delete unused language guides** from the `languages/` directory to keep context lean
   - Delete this `SETUP.md` file after completion

5. **Customize remaining configuration:**
   - Configure `Dockerfile.dev` for your language/toolchain
   - Adjust `compose.yaml` for your services
   - Refine `.gitignore` for your language

## Language-Specific Setup Guides

Detailed setup instructions for each supported language are in the `languages/` directory:

- **languages/go.md** - Go backend development
- **languages/node.md** - Node.js/TypeScript (including React Router)
- **languages/rust.md** - Rust systems programming
- **languages/python.md** - Python development
- **languages/cpp.md** - C/C++ development
- **languages/react-native.md** - React Native mobile development

### Setup Process

1. **Copy the relevant language guide to `AGENTS.md`:**
   ```bash
   cp languages/go.md AGENTS.md  # Replace with your language
   ```

2. **Remove unused language guides:**
   ```bash
   rm -rf languages/
   ```
   This keeps the agent's context clean and focused.

3. **Customize the Docker configuration:**
   Each language guide includes specific instructions for:
   - `Dockerfile.dev` - Language runtime, LSP tools, build dependencies
   - `compose.yaml` - Cache volumes, environment variables
   - `.gitignore` - Language-specific build artifacts

4. **Tailor `AGENTS.md` to your project:**
   - Replace `[PROJECT_TYPE]` and `[PROJECT_DESCRIPTION]` with your details
   - Update architecture documentation
   - Add project-specific conventions
   - Remove any language examples not relevant to your stack

## Running the LLM Environment

1. **Start the LLM container:**
   ```bash
   docker compose up -d llm
   ```

2. **Attach to the container:**
   ```bash
   docker compose exec llm zsh
   ```

3. **Run the LLM coding agent:**
   ```bash
   qwen  # or claude, or your chosen agent
   ```

## Troubleshooting

### Permission Issues
If you see permission errors creating files:
```bash
docker compose build --build-arg USER_UID=$(id -u) --build-arg USER_GID=$(id -g) llm
```

### Git Credential Prompts
Set `GIT_TERMINAL_PROMPT=0` in compose.yaml environment to fail fast instead of hanging.

### Cache Not Persisting
Check that named volumes are properly configured:
```bash
docker volume ls | grep [language]
```

## Resources

- [LLM Coding Agent Documentation](https://qwen-code.github.io/)
- [Docker Compose Reference](https://docs.docker.com/compose/)
- [Language Server Protocol](https://microsoft.github.io/language-server-protocol/)
