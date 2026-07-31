# React Native Development Environment Setup

Copy this file's contents into `AGENTS.md` and remove the `languages/` directory after setup.

---

# Agent Instructions

## Project Overview

This is a React Native application for [PROJECT_DESCRIPTION].

## Key Components and Architecture

### Core Directory Structure
- **src/** - Main source code
- **src/components/** - Reusable UI components
- **src/screens/** - Screen components (one per route)
- **src/navigation/** - Navigation configuration
- **src/hooks/** - Custom React hooks
- **src/services/** - API calls and external services
- **src/store/** - State management (Zustand, Redux, Context)
- **src/utils/** - Utility functions
- **src/types/** - TypeScript type definitions
- **src/assets/** - Images, fonts, etc.
- **src/config/** - Configuration files
- **tests/** - Test files
- **android/** - Android native code
- **ios/** - iOS native code

### React Native Best Practices

1. **Component Design**
   - Keep components small and focused
   - Separate presentation from logic (container/presentational pattern)
   - Use React Hooks for state and side effects
   - Extract reusable logic into custom hooks

2. **Performance**
   - Use `React.memo` for expensive components
   - Use `useCallback` and `useMemo` appropriately
   - Optimize FlatList with `keyExtractor` and proper `renderItem`
   - Avoid inline objects/functions in render
   - Use `Image.resolveAssetSource` for images

3. **Navigation**
   - Use React Navigation v6+
   - Use TypeScript for type-safe navigation
   - Separate navigation config from screens
   - Implement deep linking support

4. **State Management**
   - Use Zustand for simple state (recommended)
   - Use React Query/SWR for server state
   - Use Context for theme/auth
   - Avoid Redux unless necessary for complex state

## Coding Conventions

### TypeScript Setup
```typescript
// src/types/navigation.ts
export type RootStackParamList = {
  Home: undefined;
  Details: { id: string };
  Settings: undefined;
};

// src/types/api.ts
export interface User {
  id: string;
  name: string;
  email: string;
}
```

### Component Structure
```tsx
import React, { useState, useEffect } from 'react';
import { View, Text, StyleSheet } from 'react-native';

interface Props {
  title: string;
  onPress?: () => void;
}

export const MyComponent: React.FC<Props> = ({ title, onPress }) => {
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    // Cleanup if needed
    return () => {};
  }, []);

  const handlePress = () => {
    onPress?.();
  };

  return (
    <View style={styles.container}>
      <Text style={styles.title}>{title}</Text>
    </View>
  );
};

const styles = StyleSheet.create({
  container: {
    padding: 16,
  },
  title: {
    fontSize: 18,
    fontWeight: '600',
  },
});
```

## Development Workflow

### Initial Setup
```bash
# Create new React Native project (Expo)
npx create-expo-app myapp --template expo-template-blank-typescript

# Or without Expo (React Native CLI)
npx react-native init MyProject

# Navigate to project
cd myapp

# Install dependencies
npm install
# or
yarn install

# Start Metro bundler
npm start
# or
npx expo start

# Run on iOS
npm run ios

# Run on Android
npm run android
```

### Package Management
```bash
# Install packages
npm install <package>

# Install dev dependencies
npm install -D <package>

# Install Expo packages
npx expo install <package>

# Add native dependencies (requires rebuild)
npx expo install <package>
```

### Build Commands
```bash
# Development
npm start
# or
npx expo start

# iOS simulator
npm run ios

# Android emulator
npm run android

# Build for iOS (requires Xcode)
npm run ios:build

# Build for Android
npm run android:build

# Type check
npm run typecheck

# Lint
npm run lint
# or fix
npm run lint:fix

# Format
npm run format

# Run tests
npm test

# Run with coverage
npm run test:coverage
```

### Scripts Setup
```json
{
  "scripts": {
    "start": "expo start",
    "android": "expo start --android",
    "ios": "expo start --ios",
    "web": "expo start --web",
    "test": "jest",
    "test:watch": "jest --watch",
    "lint": "eslint src --ext .ts,.tsx",
    "lint:fix": "eslint src --ext .ts,.tsx --fix",
    "typecheck": "tsc --noEmit",
    "format": "prettier --write \"src/**/*.{ts,tsx}\""
  }
}
```

## Recommended Tooling

### Navigation
- **@react-navigation/native** - Core navigation
- **@react-navigation/stack** - Stack navigator
- **@react-navigation/bottom-tabs** - Tab navigator
- **@react-navigation/drawer** - Drawer navigator
- **@react-navigation/native-stack** - Native stack (recommended)

### State Management
- **Zustand** - Simple, lightweight (recommended)
- **React Query** - Server state management
- **Context API** - For theme/auth
- **Redux Toolkit** - Complex state (if needed)

### UI Libraries
- **NativeBase** - Component library
- **React Native Paper** - Material Design
- **Tamagui** - Universal UI (recommended)
- **Dripsy** - Styled-system for React Native

### Forms
- **React Hook Form** - Forms with validation
- **Yup** - Schema validation
- **Zod** - TypeScript-first validation

### Testing
- **Jest** - Test runner
- **@testing-library/react-native** - Component testing
- **Detox** - E2E testing
- **MSW** - API mocking

### Utilities
- **Axios** - HTTP client
- **React Query** - Data fetching & caching
- **MMKV** - Fast storage
- **react-native-reanimated** - Animations
- **react-native-gesture-handler** - Gestures

## TypeScript Configuration

```json
{
  "compilerOptions": {
    "target": "ESNext",
    "module": "ESNext",
    "lib": ["ESNext"],
    "allowJs": true,
    "jsx": "react-native",
    "noEmit": true,
    "isolatedModules": true,
    "strict": true,
    "moduleResolution": "node",
    "baseUrl": ".",
    "paths": {
      "@/*": ["src/*"],
      "@components/*": ["src/components/*"],
      "@screens/*": ["src/screens/*"],
      "@hooks/*": ["src/hooks/*"],
      "@utils/*": ["src/utils/*"]
    },
    "allowSyntheticDefaultImports": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "resolveJsonModule": true
  },
  "include": ["src/**/*", "tests/**/*"],
  "exclude": ["node_modules", "babel.config.js", "metro.config.js"]
}
```

## Navigation Setup

### Stack Navigator
```tsx
// src/navigation/StackNavigator.tsx
import { createNativeStackNavigator } from '@react-navigation/native-stack';
import { RootStackParamList } from '../types/navigation';

import { HomeScreen } from '@/screens/HomeScreen';
import { DetailsScreen } from '@/screens/DetailsScreen';
import { SettingsScreen } from '@/screens/SettingsScreen';

const Stack = createNativeStackNavigator<RootStackParamList>();

export const StackNavigator = () => {
  return (
    <Stack.Navigator
      initialRouteName="Home"
      screenOptions={{
        headerStyle: {
          backgroundColor: '#6200ee',
        },
        headerTintColor: '#fff',
      }}
    >
      <Stack.Screen name="Home" component={HomeScreen} />
      <Stack.Screen name="Details" component={DetailsScreen} />
      <Stack.Screen name="Settings" component={SettingsScreen} />
    </Stack.Navigator>
  );
};
```

### Tab Navigator
```tsx
// src/navigation/TabNavigator.tsx
import { createBottomTabNavigator } from '@react-navigation/bottom-tabs';
import { HomeScreen } from '@/screens/HomeScreen';
import { SearchScreen } from '@/screens/SearchScreen';
import { SettingsScreen } from '@/screens/SettingsScreen';

const Tab = createBottomTabNavigator();

export const TabNavigator = () => {
  return (
    <Tab.Navigator
      screenOptions={{
        tabBarActiveTintColor: '#6200ee',
        tabBarInactiveTintColor: 'gray',
      }}
    >
      <Tab.Screen name="Home" component={HomeScreen} />
      <Tab.Screen name="Search" component={SearchScreen} />
      <Tab.Screen name="Settings" component={SettingsScreen} />
    </Tab.Navigator>
  );
};
```

## Docker Configuration

### Dockerfile.dev Updates

```dockerfile
FROM node:20-bookworm

ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=${USER_UID}

# Install system dependencies for React Native
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        openssh-client \
        less \
        vim \
        zsh \
        python3 \
        build-essential \
        libffi-dev \
    && npm install -g @qwen-code/qwen-code@latest \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Install TypeScript and tools
RUN npm install -g typescript @types/node \
    prettier eslint

# Non-root user setup
RUN groupadd --gid ${USER_GID} ${USERNAME} \
    && useradd --uid ${USER_UID} --gid ${USER_GID} -m -s /bin/zsh ${USERNAME}

RUN mkdir -p /workspace /home/dev/.npm \
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
  NODE_ENV: development
  npm_config_cache: /home/dev/.npm
  # React Native
  RCT_NO_LAUNCH_PACKAGER: true
```

### compose.yaml Volumes

```yaml
volumes:
  - node-npm-cache:/home/dev/.npm
  - llm-home:/home/dev
```

## .gitignore Updates

```gitignore
# Dependencies
node_modules/

# Build output
dist/
build/

# Metro
.metro-health-check*

# Expo
.expo/
dist/
web-build/

# Native (generated)
*.orig.*
*.jks
*.p8
*.p12
*.key
*.mobileprovision
*.orig.*
web-build/

# macOS
.DS_Store
*.pem

# Local env files
.env.local
.env.*.local

# Testing
coverage/

# IDE
.vscode/
.idea/

# Logs
logs/
*.log
npm-debug.log*
yarn-debug.log*
yarn-error.log*
lerna-debug.log*

# TypeScript
*.tsbuildinfo
next-env.d.ts
```

## Project Structure Example

```
.
├── .expo/
├── android/
├── ios/
├── src/
│   ├── assets/
│   │   ├── images/
│   │   └── fonts/
│   ├── components/
│   │   ├── Button.tsx
│   │   ├── Input.tsx
│   │   └── Card.tsx
│   ├── config/
│   │   └── env.ts
│   ├── hooks/
│   │   ├── useAuth.ts
│   │   └── useTheme.ts
│   ├── navigation/
│   │   ├── AppNavigator.tsx
│   │   ├── StackNavigator.tsx
│   │   └── TabNavigator.tsx
│   ├── screens/
│   │   ├── HomeScreen.tsx
│   │   ├── DetailsScreen.tsx
│   │   └── SettingsScreen.tsx
│   ├── services/
│   │   ├── api.ts
│   │   └── auth.ts
│   ├── store/
│   │   ├── index.ts
│   │   ├── authSlice.ts
│   │   └── themeSlice.ts
│   ├── types/
│   │   ├── navigation.ts
│   │   └── api.ts
│   ├── utils/
│   │   ├── storage.ts
│   │   └── validation.ts
│   ├── App.tsx
│   └── main.tsx
├── tests/
│   ├── setup.ts
│   └── components/
│       └── Button.test.tsx
├── .env.example
├── app.json
├── babel.config.js
├── jest.config.js
├── metro.config.js
├── package.json
├── tsconfig.json
└── README.md
```

## State Management Example (Zustand)

```typescript
// src/store/authStore.ts
import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { User } from '@/types/api';

interface AuthState {
  user: User | null;
  token: string | null;
  isAuthenticated: boolean;
  login: (user: User, token: string) => void;
  logout: () => void;
}

export const useAuthStore = create<AuthState>()(
  persist(
    (set) => ({
      user: null,
      token: null,
      isAuthenticated: false,
      login: (user, token) => set({
        user,
        token,
        isAuthenticated: true,
      }),
      logout: () => set({
        user: null,
        token: null,
        isAuthenticated: false,
      }),
    }),
    {
      name: 'auth-storage',
      partialize: (state) => ({
        user: state.user,
        token: state.token,
      }),
    }
  )
);
```

## API Service with React Query

```typescript
// src/services/api.ts
import axios from 'axios';
import { QueryClient } from '@tanstack/react-query';
import { User } from '@/types/api';

export const api = axios.create({
  baseURL: process.env.EXPO_PUBLIC_API_URL,
  headers: {
    'Content-Type': 'application/json',
  },
});

// Add auth token to requests
api.interceptors.request.use((config) => {
  const token = useAuthStore.getState().token;
  if (token) {
    config.headers.Authorization = `Bearer ${token}`;
  }
  return config;
});

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
      retry: 1,
      staleTime: 5 * 60 * 1000, // 5 minutes
    },
  },
});

// API functions
export const userApi = {
  getCurrentUser: async (): Promise<User> => {
    const response = await api.get('/user/me');
    return response.data;
  },

  updateUser: async (data: Partial<User>): Promise<User> => {
    const response = await api.patch('/user', data);
    return response.data;
  },
};
```

## Component with React Hook Form

```tsx
// src/components/LoginForm.tsx
import React from 'react';
import { View, TextInput, Button, Text } from 'react-native';
import { useForm, Controller } from 'react-hook-form';
import { yupResolver } from '@hookform/resolvers/yup';
import * as yup from 'yup';

const schema = yup.object({
  email: yup.string().email().required(),
  password: yup.string().min(6).required(),
}).required();

type LoginFormValues = yup.InferType<typeof schema>;

export const LoginForm: React.FC = () => {
  const {
    control,
    handleSubmit,
    formState: { errors },
  } = useForm<LoginFormValues>({
    resolver: yupResolver(schema),
  });

  const onSubmit = (data: LoginFormValues) => {
    console.log(data);
  };

  return (
    <View>
      <Controller
        control={control}
        name="email"
        render={({ field: { onChange, value } }) => (
          <>
            <TextInput
              placeholder="Email"
              value={value}
              onChangeText={onChange}
              keyboardType="email-address"
              autoCapitalize="none"
            />
            {errors.email && (
              <Text style={{ color: 'red' }}>{errors.email.message}</Text>
            )}
          </>
        )}
      />

      <Controller
        control={control}
        name="password"
        render={({ field: { onChange, value } }) => (
          <>
            <TextInput
              placeholder="Password"
              value={value}
              onChangeText={onChange}
              secureTextEntry
            />
            {errors.password && (
              <Text style={{ color: 'red' }}>{errors.password.message}</Text>
            )}
          </>
        )}
      />

      <Button title="Login" onPress={handleSubmit(onSubmit)} />
    </View>
  );
};
```

## Testing Setup

### Jest Configuration
```javascript
// jest.config.js
module.exports = {
  preset: 'react-native',
  moduleFileExtensions: ['ts', 'tsx', 'js', 'jsx'],
  setupFilesAfterEnv: ['<rootDir>/tests/setup.ts'],
  transformIgnorePatterns: [
    'node_modules/(?!((@react-native|react-native|@react-navigation)/).*)',
  ],
  moduleNameMapper: {
    '^@/(.*)$': '<rootDir>/src/$1',
  },
};
```

### Test Example
```typescript
// tests/components/Button.test.tsx
import React from 'react';
import { render, fireEvent } from '@testing-library/react-native';
import { Button } from '@/components/Button';

describe('Button', () => {
  it('renders correctly', () => {
    const { getByText } = render(<Button title="Press me" />);
    expect(getByText('Press me')).toBeTruthy();
  });

  it('calls onPress when pressed', () => {
    const onPress = jest.fn();
    const { getByText } = render(<Button title="Press me" onPress={onPress} />);
    
    fireEvent.press(getByText('Press me'));
    
    expect(onPress).toHaveBeenCalledTimes(1);
  });
});
```

## Environment Configuration

### .env.example
```bash
# API Configuration
EXPO_PUBLIC_API_URL=https://api.example.com

# Feature Flags
EXPO_PUBLIC_ENABLE_DEBUG=false
EXPO_PUBLIC_FEATURE_NEW_UI=true

# API Keys (do not commit real keys)
EXPO_PUBLIC_MAPS_API_KEY=your_key_here
```

## Performance Optimization

1. **Bundle Size**
   - Use React Native Reanimated instead of Animated API
   - Lazy load screens with React Navigation
   - Use Hermes engine (enabled by default in Expo)

2. **Rendering**
   - Use `FlatList` instead of `ScrollView` for long lists
   - Implement `getItemLayout` for better performance
   - Use `React.memo` for list items
   - Virtualize long lists

3. **Images**
   - Use appropriate image sizes
   - Implement image caching
   - Use `react-native-fast-image` for better caching

## Security Best Practices

1. **Secure Storage**
   - Use `expo-secure-store` for sensitive data
   - Never store tokens in AsyncStorage
   - Implement token refresh logic

2. **API Security**
   - Use HTTPS for all API calls
   - Implement certificate pinning
   - Validate all server responses
   - Handle authentication errors gracefully

3. **Code Security**
   - Don't commit sensitive information
   - Use environment variables
   - Implement biometric authentication for sensitive actions
   - Keep dependencies updated
