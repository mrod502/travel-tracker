# C/C++ Development Environment Setup

Copy this file's contents into `AGENTS.md` and remove the `languages/` directory after setup.

---

# Agent Instructions

## Project Overview

This is a C/C++ application for [PROJECT_DESCRIPTION].

## Key Components and Architecture

### Core Directory Structure
- **src/** - Main source code
- **src/include/** - Public headers
- **src/private/** - Private headers
- **src/lib/** - Library code
- **src/bin/** - Executable sources
- **tests/** - Test files
- **tests/unit/** - Unit tests
- **tests/integration/** - Integration tests
- **benchmarks/** - Benchmark code
- **cmake/** - CMake modules and scripts
- **docs/** - Documentation
- **scripts/** - Build and utility scripts

### C++ Best Practices (C++17/20)

1. **Modern C++ Style**
   - Use C++17 or C++20 features when appropriate
   - Prefer `auto` for obvious types
   - Use `nullptr` instead of `NULL`
   - Use range-based for loops
   - Use `constexpr` for compile-time computation

2. **Memory Management**
   - Use smart pointers (`std::unique_ptr`, `std::shared_ptr`)
   - Never use raw `new`/`delete`
   - Follow RAII (Resource Acquisition Is Initialization)
   - Use `std::make_unique` and `std::make_shared`

3. **Move Semantics**
   - Implement move constructors and move assignment
   - Use `std::move` for explicit moves
   - Return by value and let RVO (Return Value Optimization) work
   - Use `std::forward` in perfect forwarding

4. **Const Correctness**
   - Use `const` for all references/pointers that don't modify
   - Mark member functions as `const` when they don't modify state
   - Use `constexpr` where possible

### C Best Practices (C11/C17)

1. **Memory Management**
   - Always check return values of `malloc`/`calloc`
   - Free memory in reverse order of allocation
   - Set pointers to `NULL` after freeing
   - Use a consistent allocation/deallocation pattern

2. **Error Handling**
   - Return error codes consistently
   - Use `errno` for system errors
   - Implement proper cleanup on error paths
   - Use `assert()` for programming errors

3. **Type Safety**
   - Use `typedef`/`struct` for custom types
   - Be explicit about pointer types
   - Avoid void pointers when possible
   - Use `_Static_assert` for compile-time checks

## Coding Conventions

### Naming (C++)
- Use `PascalCase` for class and struct names
- Use `snake_case` for functions, variables, and namespaces
- Use `SCREAMING_SNAKE_CASE` for macros and constants
- Prefix member variables with `m_` or use trailing underscore
- Use `m_name` for member variables, `name` for local variables

### Naming (C)
- Use `snake_case` for all identifiers
- Use `PREFIX_TypeName` for type names
- Use `PREFIX_function_name` for function names
- Use `PREFIX_` prefix for all public API symbols

### File Organization
- Use `.hpp`/`.h` for headers, `.cpp` for implementation
- Use include guards or `#pragma once`
- Include self as first header (catch missing includes)
- Minimize includes (forward declare when possible)

### Code Style
```cpp
// Class declaration
class MyClass {
public:
    // Constructors/Destructors
    MyClass() = default;
    ~MyClass() = default;

    // Copy/move operations
    MyClass(const MyClass&) = default;
    MyClass& operator=(const MyClass&) = default;
    MyClass(MyClass&&) = default;
    MyClass& operator=(MyClass&&) = default;

    // Public interface
    [[nodiscard]] int getValue() const noexcept { return m_value; }
    void setValue(int value) { m_value = value; }

private:
    int m_value{0};  // Default initialization
};

// Function definition
[[nodiscard]] std::optional<Result> processData(
    const std::string& input,
    int flags) noexcept(false) {
    if (input.empty()) {
        return std::nullopt;
    }

    // Process data
    return Result{/* ... */};
}
```

## Development Workflow

### Build Systems

#### CMake (Recommended)
```bash
# Create build directory
mkdir build && cd build

# Configure
cmake .. -DCMAKE_BUILD_TYPE=Debug

# Build
cmake --build .

# Run tests
ctest

# Install
cmake --install .
```

#### Make (Simple Projects)
```bash
# Build
make

# Debug build
make debug

# Clean
make clean
```

### Build Commands
```bash
# CMake configuration
cmake -S . -B build \
    -DCMAKE_BUILD_TYPE=Debug \
    -DCMAKE_EXPORT_COMPILE_COMMANDS=ON \
    -DBUILD_TESTING=ON

# Build
cmake --build build --parallel

# Build specific target
cmake --build build --target myapp

# Run tests
ctest --test-dir build --output-on-failure

# Generate documentation
doxygen Doxyfile
```

### Compiler Flags
```bash
# GCC/Clang common flags
-std=c++20           # C++20 standard
-Wall -Wextra        # Enable all warnings
-Werror              # Treat warnings as errors
-pedantic            # Strict ISO C++
-O2                  # Optimization level 2
-g                   # Debug symbols
-fPIC                # Position independent code

# Additional warnings
-Wconversion         # Type conversion warnings
-Wsign-conversion    # Sign conversion warnings
-Wcast-align         # Cast alignment warnings
-Wshadow             # Variable shadowing
-Wnull-dereference   # Null dereference warnings
-Wdouble-promotion   # Float to double promotion

# Sanitizers (debug builds)
-fsanitize=address   # Address sanitizer
-fsanitize=undefined # Undefined behavior sanitizer
-fsanitize=thread    # Thread sanitizer (not with ASan)
```

### CMakeLists.txt Example
```cmake
cmake_minimum_required(VERSION 3.20)
project(MyProject VERSION 1.0.0 LANGUAGES CXX)

# C++ Standard
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# Build options
option(BUILD_TESTS "Build tests" ON)
option(USE_SANITIZERS "Use sanitizers in debug" ON)

# Compile options
add_compile_options(
    -Wall -Wextra -Wpedantic
    $<$<CONFIG:Debug>:-g>
    $<$<CONFIG:Release>:-O3>
)

# Sanitizers
if(USE_SANITIZERS AND CMAKE_BUILD_TYPE STREQUAL "Debug")
    add_compile_options(-fsanitize=address -fsanitize=undefined)
    add_link_options(-fsanitize=address -fsanitize=undefined)
endif()

# Include directories
add_subdirectory(src)
if(BUILD_TESTS)
    add_subdirectory(tests)
endif()

# Install
install(TARGETS myapp DESTINATION bin)
```

## Recommended Tooling

### Build Systems
- **CMake** - Cross-platform build system (recommended)
- **Meson** - Modern build system
- **Bazel** - Google's build system (large projects)
- **Make** - Simple projects

### Compilers
- **GCC** (g++) - GNU Compiler Collection
- **Clang** (clang++) - LLVM compiler
- **MSVC** - Microsoft Visual C++ (Windows)

### IDEs and Editors
- **Visual Studio** - Windows (full-featured)
- **CLion** - Cross-platform (JetBrains)
- **VS Code** - Cross-platform (with C++ extension)
- **Qt Creator** - Cross-platform (lightweight)

### Language Server
- **clangd** - C/C++ language server (recommended)
- **ccls** - Alternative C/C++ language server

### Linters and Analyzers
- **clang-tidy** - C++ linter (recommended)
- **cpplint** - Google C++ style checker
- **cppcheck** - Static analyzer
- **include-what-you-use** - Check includes

### Testing
- **GoogleTest** - C++ testing framework
- **Catch2** - Modern C++ test framework (recommended)
- **doctest** - Lighter weight alternative
- **CTest** - CMake test runner

### Code Coverage
- **gcov** - GCC coverage tool
- **lcov** - gcov frontend
- **clang-cov** - Clang coverage

### Documentation
- **Doxygen** - Documentation generator
- **Sphinx** - Python-based (with breathe)
- **mdbook** - Markdown-based

### Package Managers
- **vcpkg** - Microsoft's package manager
- **conan** - C/C++ package manager
- **Hunter** - CMake-based package manager

## Docker Configuration

### Dockerfile.dev Updates

```dockerfile
FROM ubuntu:24.04

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
        cmake \
        g++ \
        clang \
        clang-tidy \
        clangd \
        doxygen \
        gdb \
        libssl-dev \
        pkg-config \
    && npm install -g @qwen-code/qwen-code@latest \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Non-root user setup
RUN groupadd --gid ${USER_GID} ${USERNAME} \
    && useradd --uid ${USER_UID} --gid ${USER_GID} -m -s /bin/zsh ${USERNAME}

RUN mkdir -p /workspace /home/dev/.cache \
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
  # Build configuration
  CXX: g++
  CC: gcc
  # Optional: cache directories
  CMAKE_BUILD_DIR: /workspace/build
```

### compose.yaml Volumes

C/C++ uses project-local `build/` directories:

```yaml
volumes:
  - llm-home:/home/dev
  # No special cache volumes needed
```

## .gitignore Updates

Ensure these C/C++-specific patterns are in `.gitignore`:

```gitignore
# Build directories
build/
cmake-build-*/
bin/
obj/
out/

# Compiled files
*.o
*.obj
*.a
*.lib
*.so
*.dylib
*.dll
*.exe
*.out
*.elf

# Visual Studio
.vs/
*.vcxproj*
*.sln
*.suo
*.user
Debug/
Release/

# CLion
cmake-build-*/
.idea/

# Xcode
*.xcodeproj
*.xcworkspace
DerivedData/

# Eclipse
.clangd/
.project
.cproject

# IDE files
.vscode/
.idea/
*.swp
*.swo
*~

# Coverage
*.gcno
*.gcda
*.gcov
coverage/
lcov/

# Testing
Testing/
test-results/

# Documentation
docs/_build/
doxygen/

# Compiled resources
*.gch
*.pyc

# CMake
CMakeCache.txt
cmake_install.cmake
Makefile
compile_commands.json

# Environment files
.env
.env.local
```

## Project Structure Example

```
.
├── .github/
│   └── workflows/
│       └── ci.yml
├── benchmarks/
│   ├── CMakeLists.txt
│   └── main.cpp
├── cmake/
│   ├── FindCustomLib.cmake
│   └── CompilerFlags.cmake
├── docs/
│   └── Doxyfile.in
├── scripts/
│   └── setup.sh
├── src/
│   ├── CMakeLists.txt
│   ├── include/
│   │   └── mylib/
│   │       ├── api.hpp
│   │       └── types.hpp
│   ├── private/
│   │   └── internal.hpp
│   └── lib/
│       ├── api.cpp
│       └── types.cpp
├── tests/
│   ├── CMakeLists.txt
│   ├── unit/
│   │   └── test_api.cpp
│   └── integration/
│       └── test_integration.cpp
├── CMakeLists.txt
├── CMakePresets.json
├── CONTRIBUTING.md
├── README.md
└── vcpkg.json
```

## Modern C++ Patterns

### Smart Pointer Usage
```cpp
#include <memory>

// Unique ownership
auto ptr = std::make_unique<MyClass>();

// Shared ownership
auto shared = std::make_shared<MyClass>();

// Factory function returning unique_ptr
[[nodiscard]] std::unique_ptr<Resource> createResource() {
    return std::make_unique<Resource>();
}
```

### Optional Return Values
```cpp
#include <optional>

[[nodiscard]] std::optional<User> findUser(int id) {
    if (auto it = users_.find(id); it != users_.end()) {
        return it->second;
    }
    return std::nullopt;
}

// Usage
if (auto user = findUser(42)) {
    std::cout << user->name << std::endl;
}
```

### Result Type (Error Handling)
```cpp
#include <expected>  // C++23, or use tsl::expected

template<typename T, typename E>
using Result = std::expected<T, E>;

[[nodiscard]] Result<int, std::string> divide(int a, int b) {
    if (b == 0) {
        return std::unexpected("Division by zero");
    }
    return a / b;
}

// C++17 alternative
struct Expected {
    int value;
    bool success;
    std::string error;
};
```

### Variant and Visit
```cpp
#include <variant>

struct Integer { int value; };
struct String { std::string value; };

using NumberOrString = std::variant<Integer, String>;

void print(NumberOrString val) {
    std::visit([](auto&& arg) {
        using T = std::decay_t<decltype(arg)>;
        if constexpr (std::is_same_v<T, Integer>) {
            std::cout << "Integer: " << arg.value << std::endl;
        } else if constexpr (std::is_same_v<T, String>) {
            std::cout << "String: " << arg.value << std::endl;
        }
    }, val);
}
```

### Range-based Algorithms (C++20)
```cpp
#include <ranges>
#include <algorithm>

auto numbers = std::vector{1, 2, 3, 4, 5, 6};

// Filter even numbers and square them
auto result = numbers
    | std::views::filter([](int n) { return n % 2 == 0; })
    | std::views::transform([](int n) { return n * n; })
    | std::ranges::to<std::vector>();
```

### String Views (Zero-copy string passing)
```cpp
#include <string_view>

// Pass string views instead of const std::string&
void processData(std::string_view data) {
    // No allocation, can accept char*, std::string, std::string_view
}
```

## C Patterns

### Error Handling with Cleanup
```c
#include <stdlib.h>
#include <string.h>

typedef struct {
    int code;
    char* message;
} Error;

typedef enum {
    OK = 0,
    ERR_INVALID_INPUT = 1,
    ERR_MEMORY = 2,
} ErrorCode;

Error* make_error(ErrorCode code, const char* message) {
    Error* err = malloc(sizeof(Error));
    if (!err) return NULL;
    
    err->code = code;
    err->message = strdup(message);
    if (!err->message) {
        free(err);
        return NULL;
    }
    return err;
}

void free_error(Error* err) {
    if (err) {
        free(err->message);
        free(err);
    }
}

int process_data(const char* input, Result* result) {
    if (!input || !result) {
        return ERR_INVALID_INPUT;
    }
    
    size_t len = strlen(input);
    char* buffer = malloc(len + 1);
    if (!buffer) {
        return ERR_MEMORY;
    }
    
    // Process data
    memcpy(buffer, input, len);
    result->data = buffer;
    
    return OK;
}
```

### RAII Pattern in C
```c
// Resource acquisition macro
#define WITH_RESOURCE(var, type, init_func, cleanup_func, ...) \
    for (type var = ({ __VA_ARGS__; init_func(); }), *_cleanup = (&var, 0); \
         var; \
         cleanup_func(var), var = 0)

// Usage
FILE* file = fopen("data.txt", "r");
if (file) {
    // file is automatically closed when scope ends
    char buffer[256];
    fread(buffer, 1, sizeof(buffer), file);
}
```

## Testing with Catch2

### CMakeLists.txt for Tests
```cmake
include(FetchContent)

FetchContent_Declare(
    Catch2
    GIT_REPOSITORY https://github.com/catchorg/Catch2.git
    GIT_TAG v3.4.0
)
FetchContent_MakeAvailable(Catch2)

add_executable(tests
    tests/unit/test_api.cpp
    tests/integration/test_integration.cpp
)

target_link_libraries(tests PRIVATE Catch2::Catch2WithMain mylib)

include(Catch)
catch_discover_tests(tests)
```

### Test Example
```cpp
#include <catch2/catch_test_macros.hpp>
#include "mylib/api.hpp"

TEST_CASE("Calculator adds correctly", "[unit]") {
    Calculator calc;
    
    SECTION("positive numbers") {
        REQUIRE(calc.add(2, 3) == 5);
    }
    
    SECTION("negative numbers") {
        REQUIRE(calc.add(-2, -3) == -5);
    }
    
    SECTION("mixed numbers") {
        REQUIRE(calc.add(-2, 3) == 1);
    }
}

TEST_CASE("divide by zero throws", "[unit]") {
    REQUIRE_THROWS_AS(divide(1, 0), std::invalid_argument);
}

TEST_CASE("find user returns optional", "[integration]") {
    Database db;
    db.add_user(1, "Alice");
    
    auto user = db.find_user(1);
    REQUIRE(user.has_value());
    REQUIRE(user.value().name == "Alice");
    
    REQUIRE_FALSE(db.find_user(999).has_value());
}
```

## Performance Considerations

1. **Profile First**
   - Use `perf` (Linux) or `Instruments` (macOS)
   - Use `valgrind` for memory analysis
   - Use `clang-tidy` for performance suggestions

2. **Optimization Strategies**
   - Enable LTO (Link-Time Optimization) for release builds
   - Use `[[nodiscard]]` for important return values
   - Use `[[maybe_unused]]` to suppress warnings
   - Use `constexpr` for compile-time computation
   - Use `std::string_view` to avoid string copies
   - Reserve vector capacity when size is known
   - Use emplace_back instead of push_back

3. **Memory Optimization**
   - Use `std::vector` instead of raw arrays
   - Use small buffer optimization (SBOT) where appropriate
   - Pool allocations for frequently allocated objects
   - Avoid unnecessary heap allocations

## Security Best Practices

1. **Input Validation**
   - Validate all input data
   - Use bounds checking (`at`, `vector::at`)
   - Avoid buffer overflows
   - Use safe string functions (`strncpy` instead of `strcpy`)

2. **Memory Safety**
   - Use smart pointers
   - Check return values of allocation functions
   - Free memory in reverse order
   - Use AddressSanitizer during development

3. **Compiler Security Flags**
```bash
# Security hardening
-fstack-protector-strong     # Stack smashing protection
-D_FORTIFY_SOURCE=2          # Buffer overflow detection
-Wformat -Wformat-security   # Format string vulnerabilities
-Wl,-z,relro,-z,now          # Full RELRO
-pie                         # Position Independent Executable
```
