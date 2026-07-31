# Go Development Environment Setup

Copy this file's contents into `AGENTS.md` and remove the `languages/` directory after setup.

---

# Agent Instructions

## Project Overview

This is a Go-based application for [PROJECT_DESCRIPTION].

## Key Components and Architecture

### Core Package Structure
- **cmd/** - Command-line interfaces and entry points
- **internal/** - Private application code (not importable by external packages)
- **pkg/** - Library code that can be imported by external projects
- **api/** - API layer (HTTP, gRPC, etc.)
- **services/** - Business logic and service implementations
- **models/** or **types/** - Data structures and types
- **repository/** or **storage/** - Data access layer
- **utils/** or **helpers/** - Utility functions

### Go Best Practices

1. **Package Organization**
   - Use `internal/` for private application code
   - Keep packages small and focused on single responsibility
   - Import paths should be meaningful and stable

2. **Error Handling**
   - Use `fmt.Errorf()` with `%w` verb for error wrapping (Go 1.13+)
   - Create custom error types when additional context is needed
   - Never ignore errors (use `_` explicitly if intentional)

3. **Context Usage**
   - Pass `context.Context` as the first parameter to functions
   - Use context for cancellation, timeouts, and request-scoped values
   - Never store contexts in structs

4. **Testing**
   - Write table-driven tests for comprehensive coverage
   - Use `t.Run()` for subtests
   - Place test files next to source files (`*_test.go`)
   - Use `go test ./...` to run all tests

## Coding Conventions

### Naming
- Use meaningful, descriptive names
- Follow Go naming conventions (exported = capitalized)
- Use short names for local variables (`i`, `err`, `ctx`)
- Use full words for exported symbols (`UserID` not `UID`)

### Formatting
- Use `gofmt` or `go fmt` for consistent formatting
- Run `goimports` to manage imports automatically
- Keep lines under 100 characters when possible

### Comments
- Document exported functions, types, and constants
- Use complete sentences starting with the symbol name
- Include package-level documentation in `doc.go` or first file

## Development Workflow

### Build Commands
```bash
# Build the application
go build -o bin/myapp ./cmd/myapp

# Build with race detector
go build -race -o bin/myapp ./cmd/myapp

# Run without building
go run ./cmd/myapp

# Run tests
go test ./...

# Run tests with coverage
go test -cover ./...

# Run tests with race detector
go test -race ./...

# Format code
go fmt ./...

# Vet for common mistakes
go vet ./...
```

### Dependencies
```bash
# Add new dependency
go get github.com/package/name

# Update dependencies
go get -u ./...

# Clean up unused dependencies
go mod tidy

# View dependency tree
go mod graph
```

## Recommended Tooling

### Linters and Analyzers
- **golangci-lint** - Aggregated linter (recommended)
- **staticcheck** - Fast, reliable static analyzer
- **govulncheck** - Check for known vulnerabilities

### Development Tools
- **gopls** - Go language server (for IDE support)
- **delve (dlv)** - Debugger
- **gotestsum** - Pretty test runner
- **air** or **fx** - Live reload for development

### Testing Libraries
- **testify** - Assertive testing toolkit (assert, mock)
- **gomock** or **moq** - Mock generation
- **ginkgo/gomega** - BDD-style testing (optional)

## Docker Configuration

### Dockerfile.dev Updates

```dockerfile
FROM golang:1.26-bookworm

ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=${USER_UID}

# Install system dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        openssh-client \
        less \
        vim \
        zsh \
    && npm install -g @qwen-code/qwen-code@latest \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Install Go tools
RUN go install golang.org/x/tools/gopls@latest \
    && go install golang.org/x/tools/gopls@latest \
    && go install github.com/golangci/golangci-lint/cmd/golangci-lint@latest \
    && go install github.com/go-delve/delve/cmd/dlv@latest

# Non-root user setup (same as template)
RUN groupadd --gid ${USER_GID} ${USERNAME} \
    && useradd --uid ${USER_UID} --gid ${USER_GID} -m -s /bin/zsh ${USERNAME}

RUN mkdir -p /go/pkg/mod /go/build-cache /go/bin /workspace \
    && chown -R ${USERNAME}:${USERNAME} /go /workspace

# Git config (same as template)
RUN git config --system --add safe.directory /workspace

ENV GIT_TERMINAL_PROMPT=0

# ... rest of template entrypoint and user setup
USER ${USERNAME}
WORKDIR /workspace
```

### compose.yaml Environment Variables

```yaml
environment:
  HOME: /home/dev
  GOPATH: /go
  GOMODCACHE: /go/pkg/mod
  GOCACHE: /go/build-cache
  GOBIN: /go/bin
  GOPRIVATE: github.com/your-org/*  # If using private modules
```

### compose.yaml Volumes

Go uses global caches, so ensure these volumes are configured:

```yaml
volumes:
  - go-mod-cache:/go/pkg/mod
  - go-build-cache:/go/build-cache
  - go-bin-cache:/go/bin
  - llm-home:/home/dev
```

## .gitignore Updates

Ensure these Go-specific patterns are in `.gitignore`:

```gitignore
# Binaries
*.exe
*.exe~
*.dll
*.so
*.dylib
myapp*  # Your binary name

# Test binary
*.test

# Coverage
*.out
coverage.html
coverage.txt

# Go workspace
go.work
go.work.sum

# Vendor (if not used)
# vendor/

# Build output
bin/
dist/
```

## Project Structure Example

```
.
├── cmd/
│   └── myapp/
│       └── main.go
├── internal/
│   ├── api/
│   ├── service/
│   └── repository/
├── pkg/
│   └── utils/
├── api/
│   └── v1/
├── tests/
├── go.mod
├── go.sum
├── Makefile
└── README.md
```

## Environment Configuration

### Recommended Approach
- Use `github.com/kelseyhightower/envconfig` for structured env parsing
- Use `.env` files for local development (add to `.gitignore`)
- Use config files (YAML/TOML) for complex configurations
- Use `viper` for config management with multiple sources

### Common Environment Variables
```bash
APP_ENV=development  # development, staging, production
APP_PORT=8080
DB_HOST=localhost
DB_PORT=5432
DB_NAME=myapp
DB_USER=postgres
DB_PASSWORD=secret
LOG_LEVEL=debug
```

## Performance Considerations

1. **Memory Management**
   - Use `sync.Pool` for frequently allocated objects
   - Avoid unnecessary allocations in hot paths
   - Use `pprof` for memory profiling

2. **Concurrency**
   - Use goroutines and channels idiomatically
   - Always handle goroutine leaks (use context cancellation)
   - Use `errgroup` for managing goroutine groups

3. **Database**
   - Use connection pooling (database/sql handles this)
   - Consider using `pgx` for PostgreSQL (better performance)
   - Use migrations (golang-migrate, gobuffalo/migrate)

## Security Best Practices

1. **Input Validation**
   - Validate all user input
   - Use `github.com/go-playground/validator` for struct validation
   - Sanitize database queries (use parameterized queries)

2. **Authentication/Authorization**
   - Use `golang-jwt/jwt` for JWT handling
   - Implement rate limiting
   - Use secure cookie settings

3. **Dependencies**
   - Run `go mod verify` regularly
   - Use `govulncheck` in CI/CD
   - Pin dependencies to specific versions

## Common Patterns

### Dependency Injection
```go
type Service struct {
    repo   repository.UserRepository
    logger *zap.Logger
}

func NewService(repo repository.UserRepository, logger *zap.Logger) *Service {
    return &Service{repo: repo, logger: logger}
}
```

### Error Handling with Context
```go
func GetUser(ctx context.Context, id string) (*User, error) {
    user, err := db.FindUser(ctx, id)
    if err != nil {
        return nil, fmt.Errorf("getting user %s: %w", id, err)
    }
    return user, nil
}
```

### HTTP Handler Pattern
```go
type Handler struct {
    service *Service
}

func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
    ctx := r.Context()
    // handle request
}
```
