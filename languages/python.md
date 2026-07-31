# Python Development Environment Setup

Copy this file's contents into `AGENTS.md` and remove the `languages/` directory after setup.

---

# Agent Instructions

## Project Overview

This is a Python application for [PROJECT_DESCRIPTION].

## Key Components and Architecture

### Core Directory Structure
- **src/** - Main source code (recommended layout)
- **src/package_name/** - Main package
- **tests/** - Test files
- **tests/unit/** - Unit tests
- **tests/integration/** - Integration tests
- **scripts/** - Utility scripts
- **docs/** - Documentation
- **migrations/** - Database migrations (if using Alembic)
- **data/** - Local data files (add to .gitignore)

### Python Best Practices

1. **Virtual Environments**
   - Always use virtual environments (venv, virtualenv, or poetry)
   - Never install packages globally for project development
   - Use `.venv` as the virtual environment directory name
   - Activate virtual environment before running commands

2. **Type Hints**
   - Use type hints for function parameters and return values
   - Use `typing` module for complex types (Optional, Union, etc.)
   - Run `mypy` for static type checking
   - Use `pydantic` for runtime type validation

3. **Code Organization**
   - Use src layout to avoid import issues
   - Keep modules focused and single-responsibility
   - Use `__init__.py` to expose public API
   - Follow PEP 8 style guidelines

4. **Error Handling**
   - Use specific exception types
   - Create custom exceptions for domain errors
   - Log errors with context
   - Never swallow exceptions silently

## Coding Conventions

### Naming
- Use `snake_case` for functions, variables, and modules
- Use `PascalCase` for classes
- Use `UPPER_CASE` for constants
- Prefix private methods with underscore (`_method`)
- Use leading double underscore for name mangling (`__method`)

### Type Hints
```python
from typing import Optional, List, Dict, Any, Union
from dataclasses import dataclass

def process_data(
    items: List[str],
    config: Optional[Dict[str, Any]] = None,
) -> Union[str, None]:
    """Process the given items."""
    pass

@dataclass
class User:
    id: int
    name: str
    email: Optional[str] = None
```

### Documentation
```python
def calculate_total(
    items: list[Item],
    tax_rate: float = 0.08,
) -> float:
    """
    Calculate the total price including tax.

    Args:
        items: List of items to calculate total for
        tax_rate: Tax rate as decimal (default: 0.08)

    Returns:
        Total price including tax

    Raises:
        ValueError: If tax_rate is negative
    """
    if tax_rate < 0:
        raise ValueError("Tax rate must be non-negative")
    subtotal = sum(item.price for item in items)
    return subtotal * (1 + tax_rate)
```

### Code Style
- Use 4 spaces for indentation
- Limit lines to 79 characters (80 total)
- Use blank lines to separate functions and classes
- Import standard library first, then third-party, then local
- Use f-strings for string formatting

## Development Workflow

### Virtual Environment Setup
```bash
# Create virtual environment
python -m venv .venv

# Activate (Unix/macOS)
source .venv/bin/activate

# Activate (Windows)
.venv\Scripts\activate

# Upgrade pip
pip install --upgrade pip
```

### Package Management
```bash
# Install packages
pip install <package>

# Install from requirements
pip install -r requirements.txt

# Freeze requirements
pip freeze > requirements.txt

# Install development dependencies
pip install -r requirements-dev.txt

# Install in editable mode (for development)
pip install -e .

# Uninstall packages
pip uninstall <package>
```

### Modern Alternative: Poetry
```bash
# Install poetry
pip install poetry

# Initialize new project
poetry init

# Install dependencies
poetry install

# Add dependency
poetry add <package>

# Add dev dependency
poetry add --group dev <package>

# Run command in virtual environment
poetry run <command>

# Build package
poetry build
```

### Build Commands
```bash
# Run type checking
mypy src/

# Run linter
ruff check src/
# or fix automatically
ruff check --fix src/

# Format code
ruff format src/
# or black
black src/

# Run tests
pytest tests/

# Run tests with coverage
pytest tests/ --cov=src --cov-report=html

# Run specific test
pytest tests/test_module.py::test_function

# Run tests with verbose output
pytest -v

# Run tests capturing output
pytest -s
```

### Setup.py / pyproject.toml
```toml
# pyproject.toml (modern approach)
[build-system]
requires = ["setuptools>=61.0", "wheel"]
build-backend = "setuptools.build_meta"

[project]
name = "myapp"
version = "0.1.0"
description = "Brief description"
readme = "README.md"
requires-python = ">=3.10"
dependencies = [
    "requests>=2.28.0",
    "pydantic>=2.0.0",
]

[project.optional-dependencies]
dev = [
    "pytest>=7.0.0",
    "pytest-cov>=4.0.0",
    "mypy>=1.0.0",
    "ruff>=0.1.0",
    "black>=23.0.0",
]

[tool.pytest.ini_options]
testpaths = ["tests"]
pythonpath = ["src"]

[tool.mypy]
python_version = "3.10"
warn_return_any = true
warn_unused_configs = true
strict = true

[tool.ruff]
target-version = "py310"
line-length = 88

[tool.black]
line-length = 88
target-version = ["py310"]
```

## Recommended Tooling

### Package Managers
- **pip** - Standard package manager
- **poetry** - Modern dependency management and packaging
- **pip-tools** - Pin dependencies with requirements.in
- **uv** - Ultra-fast Python package installer (Rust-based)

### Testing
- **pytest** - Feature-rich test framework (recommended)
- **pytest-cov** - Coverage plugin
- **pytest-asyncio** - Async test support
- **hypothesis** - Property-based testing
- **faker** - Fake data generation
- **factory-boy** - Test fixtures
- **responses** - HTTP mocking
- **monkeypatch** - Test isolation

### Linting and Formatting
- **ruff** - Fast all-in-one linter (recommended)
- **black** - Opinionated code formatter
- **isort** - Import sorting
- **mypy** - Static type checker
- **pyright** - Alternative type checker (faster)
- **pylint** - Comprehensive linter

### Web/Framework Options
- **FastAPI** - Modern async web framework (recommended)
- **Flask** - Lightweight web framework
- **Django** - Full-stack web framework
- **Starlette** - ASGI framework
- **aiohttp** - Async HTTP client/server

### Database
- **SQLAlchemy** - ORM and SQL toolkit (recommended)
- **Alembic** - Database migrations
- **asyncpg** - Async PostgreSQL driver
- **psycopg2** - PostgreSQL adapter
- **sqlite3** - SQLite (built-in)

### Validation and Serialization
- **pydantic** - Data validation using type hints (recommended)
- **pydantic-settings** - Settings management
- **marshmallow** - Object serialization/deserialization

### Async
- **asyncio** - Built-in async support
- **anyio** - Async compatibility layer
- **httpx** - Async HTTP client

### Logging and Observability
- **logging** - Built-in logging module
- **loguru** - Simplified logging
- **structlog** - Structured logging
- **sentry-sdk** - Error tracking

### Utilities
- **rich** - Rich terminal output
- **typer** - CLI creation
- **click** - CLI creation
- **tenacity** - Retry logic
- **python-dotenv** - Environment variables

## Docker Configuration

### Dockerfile.dev Updates

```dockerfile
FROM python:3.12-slim-bookworm

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
        build-essential \
        libpq-dev \
    && npm install -g @qwen-code/qwen-code@latest \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Install Python tools
RUN pip install --no-cache-dir \
        poetry \
        ruff \
        black \
        mypy \
        pytest \
        pytest-cov

# Non-root user setup
RUN groupadd --gid ${USER_GID} ${USERNAME} \
    && useradd --uid ${USER_UID} --gid ${USER_GID} -m -s /bin/zsh ${USERNAME}

# Create directories for venv and cache
RUN mkdir -p /workspace /home/dev/.cache/pip \
    && chown -R ${USERNAME}:${USERNAME} /workspace /home/dev

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
  # Python uses project-local .venv/
  VIRTUAL_ENV: /workspace/.venv
  PATH: "/workspace/.venv/bin:$PATH"
  # Optional: pip cache
  PIP_CACHE_DIR: /home/dev/.pip-cache
  # App-specific
  PYTHONPATH: "/workspace/src"
  PYTHONUNBUFFERED: "1"
  PYTHONDONTWRITEBYTECODE: "1"
```

### compose.yaml Volumes

Python uses project-local `.venv/`, so cache configuration is optional:

```yaml
volumes:
  # Optional: pip cache volume
  # - pip-cache:/home/dev/.pip-cache
  - llm-home:/home/dev
```

## .gitignore Updates

Ensure these Python-specific patterns are in `.gitignore`:

```gitignore
# Virtual environment
.venv/
venv/
ENV/
env/

# Byte-compiled / optimized / DLL files
__pycache__/
*.py[cod]
*$py.class

# C extensions
*.so

# Distribution / packaging
.Python
build/
develop-eggs/
dist/
downloads/
eggs/
.eggs/
lib/
lib64/
parts/
sdist/
var/
wheels/
*.egg-info/
.installed.cfg
*.egg

# PyInstaller
*.manifest
*.spec

# Unit test / coverage reports
htmlcov/
.tox/
.nox/
.coverage
.coverage.*
.cache
nosetests.xml
coverage.xml
*.cover
*.py,cover
.hypothesis/
.pytest_cache/

# Translations
*.mo
*.pot

# Environments
.env
.env.local
.env.*.local

# IDE
.vscode/
.idea/
*.swp
*.swo
*~

# Jupyter Notebook
.ipynb_checkpoints

# mypy
.mypy_cache/
.dmypy.json
dmypy.json

# Pyre type checker
.pyre/

# pytype static type analyzer
.pytype/
```

## Project Structure Example

```
.
├── .github/
│   └── workflows/
│       └── ci.yml
├── .venv/
├── docs/
│   └── conf.py
├── scripts/
│   └── setup.sh
├── src/
│   └── myapp/
│       ├── __init__.py
│       ├── main.py
│       ├── api/
│       │   ├── __init__.py
│       │   ├── routes.py
│       │   └── schemas.py
│       ├── core/
│       │   ├── __init__.py
│       │   ├── config.py
│       │   └── security.py
│       ├── db/
│       │   ├── __init__.py
│       │   ├── models.py
│       │   └── repository.py
│       ├── services/
│       │   └── __init__.py
│       └── utils/
│           └── __init__.py
├── tests/
│   ├── __init__.py
│   ├── conftest.py
│   ├── unit/
│   │   └── test_services.py
│   └── integration/
│       └── test_api.py
├── alembic/
│   ├── versions/
│   └── env.py
├── .env.example
├── .pre-commit-config.yaml
├── docker-compose.yml
├── Dockerfile
├── Makefile
├── poetry.lock
├── pyproject.toml
├── README.md
└── requirements.txt
```

## Configuration Management

### Using Pydantic Settings
```python
from pydantic_settings import BaseSettings, SettingsConfigDict
from typing import Optional

class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        case_sensitive=False,
    )

    # Application
    app_name: str = "MyApp"
    debug: bool = False

    # Database
    database_url: str
    database_pool_size: int = 10

    # API
    api_host: str = "0.0.0.0"
    api_port: int = 8000

    # Secrets (from environment)
    secret_key: str

settings = Settings()
```

## FastAPI Example

### Main Application
```python
from contextlib import asynccontextmanager
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel
from typing import List

# Models
class Item(BaseModel):
    id: int
    name: str
    price: float

class ItemCreate(BaseModel):
    name: str
    price: float

# Lifespan context manager
@asynccontextmanager
async def lifespan(app: FastAPI):
    # Startup
    app.state.db = await create_database()
    yield
    # Shutdown
    await app.state.db.close()

# Create app
app = FastAPI(lifespan=lifespan, title="My API")

# Routes
@app.get("/health")
async def health_check():
    return {"status": "healthy"}

@app.get("/items", response_model=List[Item])
async def list_items():
    items = await app.state.db.get_all_items()
    return items

@app.post("/items", response_model=Item)
async def create_item(item: ItemCreate):
    try:
        new_item = await app.state.db.create_item(item)
        return new_item
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))

# Exception handler
@app.exception_handler(ValueError)
async def value_exception_handler(request, exc):
    return JSONResponse(
        status_code=400,
        content={"detail": str(exc)},
    )
```

## Testing Patterns

### Pytest Configuration
```python
# tests/conftest.py
import pytest
from fastapi.testclient import TestClient
from myapp.main import app

@pytest.fixture
def client():
    with TestClient(app) as c:
        yield c

@pytest.fixture
def test_db():
    # Setup test database
    db = create_test_db()
    yield db
    # Teardown
    db.close()
```

### Unit Test Example
```python
# tests/unit/test_services.py
import pytest
from myapp.services.calculator import calculate_total
from myapp.models import Item

def test_calculate_total():
    items = [
        Item(id=1, name="Item 1", price=10.0),
        Item(id=2, name="Item 2", price=20.0),
    ]
    result = calculate_total(items, tax_rate=0.1)
    assert result == 33.0

@pytest.mark.asyncio
async def test_calculate_total_async():
    # Async test
    pass

def test_calculate_total_negative_tax():
    with pytest.raises(ValueError, match="Tax rate must be non-negative"):
        calculate_total([], tax_rate=-0.1)
```

## Performance Considerations

1. **Profile First**
   - Use `cProfile` for profiling
   - Use `line_profiler` for line-by-line analysis
   - Use `memory_profiler` for memory usage

2. **Optimization Strategies**
   - Use async I/O for I/O-bound operations
   - Use multiprocessing for CPU-bound operations
   - Use caching (functools.lru_cache, Redis)
   - Use database connection pooling
   - Consider Cython or Numba for heavy computations

3. **Async Patterns**
```python
import asyncio
import httpx

async def fetch_multiple_urls(urls: list[str]) -> dict[str, str]:
    async with httpx.AsyncClient() as client:
        tasks = [fetch_url(client, url) for url in urls]
        results = await asyncio.gather(*tasks)
        return dict(zip(urls, results))

async def fetch_url(client: httpx.AsyncClient, url: str) -> str:
    response = await client.get(url)
    return response.text
```

## Security Best Practices

1. **Input Validation**
   - Use pydantic for request validation
   - Sanitize file uploads
   - Validate user input server-side
   - Use parameterized queries (SQLAlchemy does this)

2. **Secrets Management**
   - Never commit secrets to version control
   - Use environment variables or secret managers
   - Rotate secrets regularly
   - Use hashed passwords (bcrypt, argon2)

3. **Dependencies**
   - Run `safety check` or `pip-audit` regularly
   - Use `pip-tools` to pin dependencies
   - Review dependencies for vulnerabilities
   - Keep dependencies updated

4. **Security Headers**
```python
from fastapi.middleware import Middleware
from fastapi.middleware.cors import CORSMiddleware
from starlette.middleware.trustedhost import TrustedHostMiddleware

app.add_middleware(CORSMiddleware,
    allow_origins=["https://example.com"],
    allow_credentials=True,
    allow_methods=["GET", "POST"],
    allow_headers=["*"],
)

app.add_middleware(TrustedHostMiddleware,
    allowed_hosts=["example.com", "*.example.com"]
)
```
