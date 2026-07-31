# Node.js/TypeScript Development Environment Setup

Copy this file's contents into `AGENTS.md` and remove the `languages/` directory after setup.

---

# Agent Instructions

## Project Overview

This is a Node.js/TypeScript application for [PROJECT_DESCRIPTION].

## Key Components and Architecture

### Core Directory Structure
- **src/** - Main source code directory
- **src/components/** - React components (if using React)
- **src/pages/** - Page components (React Router)
- **src/routes/** - Route definitions (React Router)
- **src/hooks/** - Custom React hooks
- **src/services/** - API calls and external service integrations
- **src/store/** - State management (Zustand, Redux, Context)
- **src/utils/** - Utility functions
- **src/types/** - TypeScript type definitions
- **src/styles/** - Styles (CSS, CSS Modules, styled-components)
- **tests/** or **__tests__/** - Test files
- **public/** - Static assets

### TypeScript Best Practices

1. **Type Safety**
   - Enable strict mode in `tsconfig.json`
   - Avoid `any` type - use `unknown` when type is uncertain
   - Define interfaces for all props and data structures
   - Use generic types for reusable components

2. **Module Organization**
   - Use ES modules (`import`/`export`)
   - Use absolute imports with path aliases
   - Barrel files (`index.ts`) for clean exports
   - Co-locate tests with source files

3. **Type Definitions**
   - Create type guards for runtime type checking
   - Use discriminated unions for state management
   - Prefer interfaces for object shapes
   - Use type aliases for unions and primitives

## Coding Conventions

### Naming
- Use `camelCase` for variables, functions, and properties
- Use `PascalCase` for classes, interfaces, and components
- Use `UPPER_CASE` for constants
- Prefix event handlers with `on` (e.g., `onClick`, `onChange`)
- Prefix boolean variables with `is`, `has`, `should`

### Component Structure (React)
```tsx
// 1. Imports
import React, { useState, useEffect } from 'react';

// 2. Types/Interfaces
interface Props {
  title: string;
  onClick?: () => void;
}

// 3. Component
export const MyComponent: React.FC<Props> = ({ title, onClick }) => {
  // 4. Hooks
  const [state, setState] = useState<DataType>();

  // 5. Effects
  useEffect(() => {
    // cleanup if needed
    return () => {};
  }, [dependencies]);

  // 6. Event handlers
  const handleClick = () => {
    onClick?.();
  };

  // 7. Render
  return <div onClick={handleClick}>{title}</div>;
};
```

### Code Style
- Use functional components with hooks
- Keep components small and focused (Single Responsibility)
- Extract custom hooks for reusable logic
- Use destructuring for props and state
- Prefer early returns to reduce nesting

## Development Workflow

### Package Management
```bash
# Install dependencies
npm install
# or
yarn install
# or
pnpm install

# Install dev dependencies
npm install -D <package>

# Update dependencies
npm update

# Check for outdated packages
npm outdated

# Remove unused dependencies
npm prune
```

### Build Commands
```bash
# Development server
npm run dev
# or
yarn dev

# Build for production
npm run build
# or
yarn build

# Preview production build
npm run preview

# Run tests
npm test
# or with coverage
npm run test:coverage

# Lint code
npm run lint
# or fix automatically
npm run lint:fix

# Type check
npm run typecheck

# Format code
npm run format
# or fix automatically
npm run format:fix
```

### Scripts Setup
Add these to `package.json`:
```json
{
  "scripts": {
    "dev": "vite",
    "build": "tsc && vite build",
    "preview": "vite preview",
    "lint": "eslint src --ext .ts,.tsx",
    "lint:fix": "eslint src --ext .ts,.tsx --fix",
    "typecheck": "tsc --noEmit",
    "test": "vitest",
    "test:coverage": "vitest --coverage",
    "format": "prettier --write \"src/**/*.{ts,tsx}\""
  }
}
```

## Recommended Tooling

### Framework Options
- **Vite** - Fast build tool and dev server (recommended)
- **Next.js** - Full-featured React framework (SSR, SSG)
- **Remix** - Web standards-focused framework
- **Create React App** - Legacy (not recommended for new projects)

### Router
- **React Router v6+** - Most popular router
  - Use `createBrowserRouter` for data routers
  - Use nested routes with `Outlet`
  - Use `useLoaderData` and `useActionData`

### State Management
- **Zustand** - Simple, lightweight (recommended)
- **Jotai** - Atomic state management
- **Redux Toolkit** - Full-featured (for complex apps)
- **React Context** - For low-frequency updates

### UI Components
- **shadcn/ui** - Reusable components (recommended)
- **Radix UI** - Unstyled, accessible primitives
- **Mantine** - Feature-rich component library
- **Chakra UI** - Styling-focused components

### Styling
- **Tailwind CSS** - Utility-first (recommended)
- **CSS Modules** - Scoped CSS
- **styled-components** - CSS-in-JS
- **vanilla-extract** - Type-safe CSS

### Testing
- **Vitest** - Fast test runner (Vite-native)
- **Jest** - Feature-rich test runner
- **React Testing Library** - Component testing
- **Playwright** - E2E testing
- **MSW** - API mocking

### Linting and Formatting
- **ESLint** - Code linting
- **Prettier** - Code formatting
- **typescript-eslint** - TypeScript ESLint rules
- **eslint-plugin-react-hooks** - React hooks rules
- **eslint-plugin-jsx-a11y** - Accessibility

### TypeScript Configuration
```json
{
  "compilerOptions": {
    "target": "ES2020",
    "lib": ["ES2020", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "resolveJsonModule": true,
    "isolatedModules": true,
    "noEmit": true,
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "baseUrl": ".",
    "paths": {
      "@/*": ["./src/*"]
    }
  },
  "include": ["src"],
  "references": [{ "path": "./tsconfig.node.json" }]
}
```

## Docker Configuration

### Dockerfile.dev Updates

```dockerfile
FROM node:20-bookworm

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

# Install TypeScript and language server
RUN npm install -g typescript @types/node typescript-language-server \
    && npm install -g prettier eslint

# Non-root user setup (same as template)
RUN groupadd --gid ${USER_GID} ${USERNAME} \
    && useradd --uid ${USER_UID} --gid ${USER_GID} -m -s /bin/zsh ${USERNAME}

# Create directories for npm cache
RUN mkdir -p /home/dev/.npm /workspace \
    && chown -R ${USERNAME}:${USERNAME} /home/dev /workspace

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
  NODE_ENV: development
  npm_config_cache: /home/dev/.npm
  # For Vite
  VITE_API_URL: ${VITE_API_URL:-http://localhost:3000}
```

### compose.yaml Volumes

Node.js uses project-local `node_modules/`, so cache configuration is optional:

```yaml
# Optional: npm cache volume (can comment out if not needed)
# - node-npm-cache:/home/dev/.npm
- llm-home:/home/dev
```

## .gitignore Updates

Ensure these Node.js-specific patterns are in `.gitignore`:

```gitignore
# Dependencies
node_modules/

# Build output
dist/
build/
.next/
out/

# TypeScript cache
*.tsbuildinfo
.tsbuildinfo

# IDE
.vscode/
.idea/

# Testing
coverage/
.nyc_output/

# Environment files
.env
.env.local
.env.development.local
.env.test.local
.env.production.local

# Logs
logs/
*.log
npm-debug.log*

# OS
.DS_Store
Thumbs.db
```

## Project Structure Example (React Router)

```
.
├── public/
│   └── favicon.svg
├── src/
│   ├── components/
│   │   ├── ui/           # Base UI components
│   │   ├── layout/       # Layout components
│   │   └── features/     # Feature-specific components
│   ├── hooks/
│   │   └── useAuth.ts
│   ├── lib/
│   │   └── api.ts        # API client
│   ├── pages/
│   │   ├── HomePage.tsx
│   │   ├── AboutPage.tsx
│   │   └── NotFoundPage.tsx
│   ├── routes/
│   │   └── index.tsx     # Route definitions
│   ├── styles/
│   │   └── globals.css
│   ├── types/
│   │   └── api.ts
│   ├── utils/
│   │   └── helpers.ts
│   ├── App.tsx
│   ├── main.tsx
│   └── vite-env.d.ts
├── tests/
│   └── setup.ts
├── .env.example
├── index.html
├── package.json
├── tsconfig.json
├── tsconfig.node.json
├── vite.config.ts
└── tailwind.config.js
```

## React Router Best Practices

### Route Setup
```tsx
import { createBrowserRouter, RouterProvider } from 'react-router-dom';
import { Layout } from '@/components/layout/Layout';
import { HomePage } from '@/pages/HomePage';
import { AboutPage } from '@/pages/AboutPage';
import { authLoader } from '@/lib/auth';

const router = createBrowserRouter([
  {
    path: '/',
    element: <Layout />,
    errorElement: <ErrorPage />,
    children: [
      {
        index: true,
        element: <HomePage />,
      },
      {
        path: 'about',
        element: <AboutPage />,
        loader: authLoader,
      },
    ],
  },
]);

export default function App() {
  return <RouterProvider router={router} />;
}
```

### Loader/Action Pattern
```tsx
export async function loader({ params }: LoaderFunctionArgs) {
  try {
    const data = await api.getPost(params.id);
    return json({ post: data });
  } catch (error) {
    throw new Response('Not found', { status: 404 });
  }
}

export async function action({ request }: ActionFunctionArgs) {
  const formData = await request.formData();
  const data = Object.fromEntries(formData);
  await api.updatePost(data);
  return redirect('/posts');
}

// In component
export const PostPage = () => {
  const { post } = useLoaderData<typeof loader>();
  return <div>{post.title}</div>;
};
```

## Environment Configuration

### .env.example
```bash
# API Configuration
VITE_API_URL=http://localhost:3000

# Feature Flags
VITE_ENABLE_DEBUG=false
VITE_FEATURE_NEW_UI=true
```

### Using Environment Variables
```tsx
// Access in code
const apiUrl = import.meta.env.VITE_API_URL;
```

## Performance Considerations

1. **Bundle Optimization**
   - Use code splitting with dynamic imports
   - Analyze bundle size with `rollup-plugin-visualizer`
   - Tree shake unused code
   - Lazy load heavy components

2. **Rendering Optimization**
   - Use `React.memo` for expensive components
   - Use `useMemo` and `useCallback` appropriately
   - Implement virtualization for long lists
   - Debounce/throttle expensive operations

3. **Network Optimization**
   - Implement SWR or React Query for data fetching
   - Use HTTP/2 or HTTP/3
   - Implement caching strategies
   - Optimize images (use WebP, lazy loading)

## Security Best Practices

1. **XSS Prevention**
   - Never use `dangerouslySetInnerHTML` with user input
   - Sanitize user input before rendering
   - Use Content Security Policy (CSP) headers

2. **Authentication**
   - Use HTTP-only cookies for tokens
   - Implement CSRF protection
   - Use secure session management
   - Validate tokens server-side

3. **Dependency Security**
   - Run `npm audit` regularly
   - Use `npm audit fix` to update vulnerable packages
   - Consider `Snyk` or `Dependabot` for monitoring
