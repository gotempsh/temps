import { ProjectResponse } from '@/api/client/types.gen'
import {
  getOrCreateDsnMutation,
  hasErrorGroupsOptions,
  listDsnsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  SetupWizardShell,
  WizardStepId,
} from '@/components/project/setup/SetupWizardShell'
import { Button } from '@/components/ui/button'
import { CodeBlock } from '@/components/ui/code-block'
import { CopyButton } from '@/components/ui/copy-button'
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  ArrowRight,
  Check,
  FileCode,
  Info,
  Loader2,
  Sparkles,
  Terminal,
} from 'lucide-react'
import { ReactNode, useEffect, useMemo, useState } from 'react'
import { useNavigate } from 'react-router'

interface ErrorTrackingSetupProps {
  project: ProjectResponse
}

type PlatformId =
  | 'react'
  | 'nextjs'
  | 'vue'
  | 'svelte'
  | 'angular'
  | 'javascript'
  | 'nodejs'
  | 'python'
  | 'go'
  | 'rust'
  | 'ruby'
  | 'java'
  | 'php'
  | 'dotnet'
  | 'reactnative'
  | 'flutter'

type CodeLanguage =
  | 'javascript'
  | 'typescript'
  | 'python'
  | 'go'
  | 'text'

type PlatformCategory = 'fullstack' | 'frontend' | 'backend' | 'mobile'

interface Platform {
  id: PlatformId
  name: string
  description: string
  category: PlatformCategory
  packageName: string
  installCommand: string
  language: CodeLanguage
  envVarName: string
  dsnExpression: string
  sentrySkill?: string
  icon: ReactNode
  buildSnippet: (dsnExpr: string) => string
}

const CATEGORY_ORDER: PlatformCategory[] = [
  'fullstack',
  'frontend',
  'backend',
  'mobile',
]

const CATEGORY_META: Record<
  PlatformCategory,
  {
    label: string
    hint: string
    accent: string
    badgeClass: string
  }
> = {
  fullstack: {
    label: 'Full-stack frameworks',
    hint: 'One SDK captures both browser and server errors from the same app',
    accent: 'border-l-foreground',
    badgeClass: 'bg-muted text-muted-foreground',
  },
  frontend: {
    label: 'Frontend',
    hint: 'Runs in the browser — captures client-side JS errors, replay, and perf',
    accent: 'border-l-border',
    badgeClass: 'bg-muted text-muted-foreground',
  },
  backend: {
    label: 'Backend',
    hint: 'Runs on the server — captures API, worker, and cron exceptions',
    accent: 'border-l-border',
    badgeClass: 'bg-muted text-muted-foreground',
  },
  mobile: {
    label: 'Mobile',
    hint: 'iOS, Android, and cross-platform native apps',
    accent: 'border-l-border',
    badgeClass: 'bg-muted text-muted-foreground',
  },
}

const PRESET_TO_PLATFORM: Record<string, PlatformId> = {
  nextjs: 'nextjs',
  nuxt: 'vue',
  vue: 'vue',
  vite: 'react',
  react: 'react',
  'react-app': 'react',
  remix: 'react',
  svelte: 'svelte',
  solid: 'javascript',
  astro: 'javascript',
  express: 'nodejs',
  nestjs: 'nodejs',
  nodejs_generic: 'nodejs',
  rsbuild: 'react',
  docusaurus: 'react',
  python: 'python',
  python_generic: 'python',
  django: 'python',
  flask: 'python',
  fastapi: 'python',
  streamlit: 'python',
  rust: 'rust',
  go: 'go',
  java_generic: 'java',
  maven: 'java',
  gradle: 'java',
}

function platformForPreset(preset?: string | null): PlatformId {
  if (!preset) return 'react'
  return PRESET_TO_PLATFORM[preset] ?? 'react'
}

const JsIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M0 0h24v24H0V0zm22.034 18.276c-.175-1.095-.888-2.015-3.003-2.873-.736-.345-1.554-.585-1.797-1.14-.091-.33-.105-.51-.046-.705.15-.646.915-.84 1.515-.66.39.12.75.42.976.9 1.034-.676 1.034-.676 1.755-1.125-.27-.42-.404-.601-.586-.78-.63-.705-1.469-1.065-2.834-1.034l-.705.089c-.676.165-1.32.525-1.71 1.005-1.14 1.291-.811 3.541.569 4.471 1.365 1.02 3.361 1.244 3.616 2.205.24 1.17-.87 1.545-1.966 1.41-.811-.18-1.26-.586-1.755-1.336l-1.83 1.051c.21.48.45.689.81 1.109 1.74 1.756 6.09 1.666 6.871-1.004.029-.09.24-.705.074-1.65l.046.067zm-8.983-7.245h-2.248c0 1.938-.009 3.864-.009 5.805 0 1.232.063 2.363-.138 2.711-.33.689-1.18.601-1.566.48-.396-.196-.597-.466-.83-.855-.063-.105-.11-.196-.127-.196l-1.825 1.125c.305.63.75 1.172 1.324 1.517.855.51 2.004.675 3.207.405.783-.226 1.458-.691 1.811-1.411.51-.93.402-2.07.397-3.346.012-2.054 0-4.109 0-6.179l.004-.056z" />
  </svg>
)

const ReactLogo = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M14.23 12.004a2.236 2.236 0 0 1-2.235 2.236 2.236 2.236 0 0 1-2.236-2.236 2.236 2.236 0 0 1 2.235-2.236 2.236 2.236 0 0 1 2.236 2.236zm2.648-10.69c-1.346 0-3.107.96-4.888 2.622-1.78-1.653-3.542-2.602-4.887-2.602-.41 0-.783.093-1.106.278-1.375.793-1.683 3.264-.973 6.365C1.98 8.917 0 10.42 0 12.004c0 1.59 1.99 3.097 5.043 4.03-.704 3.113-.39 5.588.988 6.38.32.187.69.275 1.102.275 1.345 0 3.107-.96 4.888-2.624 1.78 1.654 3.542 2.603 4.887 2.603.41 0 .783-.09 1.106-.275 1.374-.792 1.683-3.263.973-6.365C22.02 15.096 24 13.59 24 12.004c0-1.59-1.99-3.097-5.043-4.032.704-3.11.39-5.587-.988-6.38a2.167 2.167 0 0 0-1.092-.278zm-.005 1.09v.006c.225 0 .406.044.558.127.666.382.955 1.835.73 3.704-.054.46-.142.945-.25 1.44a23.476 23.476 0 0 0-3.107-.534A23.892 23.892 0 0 0 12.769 4.62c1.055-.98 2.047-1.524 2.86-1.524zM6.21 2.396c.154 0 .32.02.52.075.654.228 1.23.915 1.704 1.836a19.807 19.807 0 0 0-2.04 2.452 20.004 20.004 0 0 0-3.098.536c-.112-.49-.195-.964-.254-1.42-.23-1.868.054-3.32.714-3.707.19-.09.4-.127.563-.127z" />
  </svg>
)

const NodeIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M11.998,24c-0.321,0-0.641-0.084-0.922-0.247l-2.936-1.737c-0.438-0.245-0.224-0.332-0.08-0.383 c0.585-0.203,0.703-0.25,1.328-0.604c0.065-0.037,0.151-0.023,0.218,0.017l2.256,1.339c0.082,0.045,0.197,0.045,0.272,0l8.795-5.076 c0.082-0.047,0.134-0.141,0.134-0.238V6.921c0-0.099-0.053-0.192-0.137-0.242l-8.791-5.072c-0.081-0.047-0.189-0.047-0.271,0 L3.075,6.68C2.99,6.729,2.936,6.825,2.936,6.921v10.15c0,0.097,0.054,0.189,0.139,0.235l2.409,1.392 c1.307,0.654,2.108-0.116,2.108-0.89V7.787c0-0.142,0.114-0.253,0.256-0.253h1.115c0.139,0,0.255,0.112,0.255,0.253v10.021 c0,1.745-0.95,2.745-2.604,2.745c-0.508,0-0.909,0-2.026-0.551L2.28,18.675c-0.57-0.329-0.922-0.945-0.922-1.604V6.921 c0-0.659,0.353-1.275,0.922-1.603l8.795-5.082c0.557-0.315,1.296-0.315,1.848,0l8.794,5.082c0.57,0.329,0.924,0.944,0.924,1.603 v10.15c0,0.659-0.354,1.275-0.924,1.604l-8.794,5.078C12.643,23.916,12.324,24,11.998,24z M19.099,13.993 c0-1.9-1.284-2.406-3.987-2.763c-2.731-0.361-3.009-0.548-3.009-1.187c0-0.528,0.235-1.233,2.258-1.233 c1.807,0,2.473,0.389,2.747,1.607c0.024,0.115,0.129,0.199,0.247,0.199h1.141c0.071,0,0.138-0.031,0.186-0.081 c0.048-0.054,0.074-0.123,0.067-0.196c-0.177-2.098-1.571-3.076-4.388-3.076c-2.508,0-4.004,1.058-4.004,2.833 c0,1.925,1.488,2.457,3.895,2.695c2.88,0.282,3.103,0.703,3.103,1.269c0,0.983-0.789,1.402-2.642,1.402 c-2.327,0-2.839-0.584-3.011-1.742c-0.02-0.124-0.126-0.215-0.253-0.215h-1.137c-0.141,0-0.254,0.112-0.254,0.253 c0,1.482,0.806,3.248,4.655,3.248C17.501,17.007,19.099,15.91,19.099,13.993z" />
  </svg>
)

const PythonIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M14.25.18l.9.2.73.26.59.3.45.32.34.34.25.34.16.33.1.3.04.26.02.2-.01.13V8.5l-.05.63-.13.55-.21.46-.26.38-.3.31-.33.25-.35.19-.35.14-.33.1-.3.07-.26.04-.21.02H8.77l-.69.05-.59.14-.5.22-.41.27-.33.32-.27.35-.2.36-.15.37-.1.35-.07.32-.04.27-.02.21v3.06H3.17l-.21-.03-.28-.07-.32-.12-.35-.18-.36-.26-.36-.36-.35-.46-.32-.59-.28-.73-.21-.88-.14-1.05-.05-1.23.06-1.22.16-1.04.24-.87.32-.71.36-.57.4-.44.42-.33.42-.24.4-.16.36-.1.32-.05.24-.01h.16l.06.01h8.16v-.83H6.18l-.01-2.75-.02-.37.05-.34.11-.31.17-.28.25-.26.31-.23.38-.2.44-.18.51-.15.58-.12.64-.1.71-.06.77-.04.84-.02 1.27.05zm-6.3 1.98l-.23.33-.08.41.08.41.23.34.33.22.41.09.41-.09.33-.22.23-.34.08-.41-.08-.41-.23-.33-.33-.22-.41-.09-.41.09zm13.09 3.95l.28.06.32.12.35.18.36.27.36.35.35.47.32.59.28.73.21.88.14 1.04.05 1.23-.06 1.23-.16 1.04-.24.86-.32.71-.36.57-.4.45-.42.33-.42.24-.4.16-.36.09-.32.05-.24.02-.16-.01h-8.22v.82h5.84l.01 2.76.02.36-.05.34-.11.31-.17.29-.25.25-.31.24-.38.2-.44.17-.51.15-.58.13-.64.09-.71.07-.77.04-.84.01-1.27-.04-1.07-.14-.9-.2-.73-.25-.59-.3-.45-.33-.34-.34-.25-.34-.16-.33-.1-.3-.04-.25-.02-.2.01-.13v-5.34l.05-.64.13-.54.21-.46.26-.38.3-.32.33-.24.35-.2.35-.14.33-.1.3-.06.26-.04.21-.02.13-.01h5.84l.69-.05.59-.14.5-.21.41-.28.33-.32.27-.35.2-.36.15-.36.1-.35.07-.32.04-.28.02-.21V6.07h2.09l.14.01zm-6.47 14.25l-.23.33-.08.41.08.41.23.33.33.23.41.08.41-.08.33-.23.23-.33.08-.41-.08-.41-.23-.33-.33-.23-.41-.08-.41.08z" />
  </svg>
)

const MonoLetterIcon = ({ letter }: { letter: string }) => (
  <div className="flex size-5 items-center justify-center rounded bg-foreground text-[10px] font-bold text-background">
    {letter}
  </div>
)

const PLATFORMS: Platform[] = [
  {
    id: 'react',
    name: 'React',
    description: 'Browser SPA',
    category: 'frontend',
    packageName: '@sentry/react',
    installCommand: 'npm install @sentry/react',
    language: 'javascript',
    envVarName: 'VITE_SENTRY_DSN',
    dsnExpression: 'import.meta.env.VITE_SENTRY_DSN',
    sentrySkill: 'sentry-react-sdk',
    icon: <ReactLogo />,
    buildSnippet: (dsnExpr) => `import * as Sentry from "@sentry/react";

Sentry.init({
  dsn: ${dsnExpr},
  integrations: [Sentry.replayIntegration()],
  tracesSampleRate: 1.0,
  replaysSessionSampleRate: 0.1,
  replaysOnErrorSampleRate: 1.0,
});`,
  },
  {
    id: 'nextjs',
    name: 'Next.js',
    description: 'App Router or Pages',
    category: 'fullstack',
    packageName: '@sentry/nextjs',
    installCommand: 'npx @sentry/wizard@latest -i nextjs',
    language: 'javascript',
    envVarName: 'NEXT_PUBLIC_SENTRY_DSN',
    dsnExpression: 'process.env.NEXT_PUBLIC_SENTRY_DSN',
    sentrySkill: 'sentry-nextjs-sdk',
    icon: <MonoLetterIcon letter="N" />,
    buildSnippet: (dsnExpr) => `// sentry.client.config.ts
import * as Sentry from "@sentry/nextjs";

Sentry.init({
  dsn: ${dsnExpr},
  tracesSampleRate: 1.0,
  replaysSessionSampleRate: 0.1,
  replaysOnErrorSampleRate: 1.0,
  integrations: [Sentry.replayIntegration()],
});`,
  },
  {
    id: 'vue',
    name: 'Vue',
    description: 'Vue 3 / Nuxt',
    category: 'frontend',
    packageName: '@sentry/vue',
    installCommand: 'npm install @sentry/vue',
    language: 'javascript',
    envVarName: 'VITE_SENTRY_DSN',
    dsnExpression: 'import.meta.env.VITE_SENTRY_DSN',
    icon: <MonoLetterIcon letter="V" />,
    buildSnippet: (dsnExpr) => `import { createApp } from "vue";
import * as Sentry from "@sentry/vue";
import App from "./App.vue";

const app = createApp(App);

Sentry.init({
  app,
  dsn: ${dsnExpr},
  tracesSampleRate: 1.0,
});

app.mount("#app");`,
  },
  {
    id: 'svelte',
    name: 'Svelte',
    description: 'Svelte / SvelteKit',
    category: 'fullstack',
    packageName: '@sentry/sveltekit',
    installCommand: 'npx @sentry/wizard@latest -i sveltekit',
    language: 'javascript',
    envVarName: 'PUBLIC_SENTRY_DSN',
    dsnExpression: 'PUBLIC_SENTRY_DSN',
    icon: <MonoLetterIcon letter="S" />,
    buildSnippet: (dsnExpr) => `// src/hooks.client.ts
import * as Sentry from "@sentry/sveltekit";
import { PUBLIC_SENTRY_DSN } from "$env/static/public";

Sentry.init({
  dsn: ${dsnExpr},
  tracesSampleRate: 1.0,
});

export const handleError = Sentry.handleErrorWithSentry();`,
  },
  {
    id: 'angular',
    name: 'Angular',
    description: 'Angular 16+',
    category: 'frontend',
    packageName: '@sentry/angular',
    installCommand: 'npm install @sentry/angular',
    language: 'typescript',
    envVarName: 'SENTRY_DSN',
    dsnExpression: 'environment.sentryDsn',
    icon: <MonoLetterIcon letter="A" />,
    buildSnippet: (dsnExpr) => `// src/main.ts
import * as Sentry from "@sentry/angular";
import { environment } from "./environments/environment";

Sentry.init({
  dsn: ${dsnExpr},
  tracesSampleRate: 1.0,
});`,
  },
  {
    id: 'javascript',
    name: 'JavaScript',
    description: 'Vanilla browser',
    category: 'frontend',
    packageName: '@sentry/browser',
    installCommand: 'npm install @sentry/browser',
    language: 'javascript',
    envVarName: 'VITE_SENTRY_DSN',
    dsnExpression: 'import.meta.env.VITE_SENTRY_DSN',
    sentrySkill: 'sentry-browser-sdk',
    icon: <JsIcon />,
    buildSnippet: (dsnExpr) => `import * as Sentry from "@sentry/browser";

Sentry.init({
  dsn: ${dsnExpr},
  integrations: [Sentry.browserTracingIntegration(), Sentry.replayIntegration()],
  tracesSampleRate: 1.0,
  replaysSessionSampleRate: 0.1,
  replaysOnErrorSampleRate: 1.0,
});`,
  },
  {
    id: 'nodejs',
    name: 'Node.js',
    description: 'Express, Fastify, NestJS',
    category: 'backend',
    packageName: '@sentry/node',
    installCommand: 'npm install @sentry/node',
    language: 'javascript',
    envVarName: 'SENTRY_DSN',
    dsnExpression: 'process.env.SENTRY_DSN',
    sentrySkill: 'sentry-node-sdk',
    icon: <NodeIcon />,
    buildSnippet: (dsnExpr) => `// Must be the first import in your entrypoint.
import * as Sentry from "@sentry/node";

Sentry.init({
  dsn: ${dsnExpr},
  environment: process.env.NODE_ENV,
  tracesSampleRate: 1.0,
});`,
  },
  {
    id: 'python',
    name: 'Python',
    description: 'Flask, Django, FastAPI',
    category: 'backend',
    packageName: 'sentry-sdk',
    installCommand: 'pip install sentry-sdk',
    language: 'python',
    envVarName: 'SENTRY_DSN',
    dsnExpression: 'os.environ["SENTRY_DSN"]',
    icon: <PythonIcon />,
    buildSnippet: (dsnExpr) => `import os
import sentry_sdk

sentry_sdk.init(
    dsn=${dsnExpr},
    environment=os.environ.get("ENV", "development"),
    traces_sample_rate=1.0,
    profiles_sample_rate=1.0,
)`,
  },
  {
    id: 'go',
    name: 'Go',
    description: 'net/http, Gin, Echo',
    category: 'backend',
    packageName: 'github.com/getsentry/sentry-go',
    installCommand: 'go get github.com/getsentry/sentry-go',
    language: 'go',
    envVarName: 'SENTRY_DSN',
    dsnExpression: 'os.Getenv("SENTRY_DSN")',
    icon: <MonoLetterIcon letter="Go" />,
    buildSnippet: (dsnExpr) => `package main

import (
    "log"
    "os"

    "github.com/getsentry/sentry-go"
)

func main() {
    err := sentry.Init(sentry.ClientOptions{
        Dsn:              ${dsnExpr},
        TracesSampleRate: 1.0,
        Environment:      os.Getenv("ENV"),
    })
    if err != nil {
        log.Fatalf("sentry.Init: %s", err)
    }
    defer sentry.Flush(2 * time.Second)
}`,
  },
  {
    id: 'rust',
    name: 'Rust',
    description: 'Axum, Actix, Tokio',
    category: 'backend',
    packageName: 'sentry',
    installCommand: 'cargo add sentry sentry-tracing',
    language: 'text',
    envVarName: 'SENTRY_DSN',
    dsnExpression: 'std::env::var("SENTRY_DSN").unwrap()',
    icon: <MonoLetterIcon letter="Rs" />,
    buildSnippet: (dsnExpr) => `use std::env;

fn main() {
    let _guard = sentry::init((
        ${dsnExpr},
        sentry::ClientOptions {
            release: sentry::release_name!(),
            traces_sample_rate: 1.0,
            environment: env::var("ENV").ok().map(Into::into),
            ..Default::default()
        },
    ));

    // Your app entrypoint
}`,
  },
  {
    id: 'ruby',
    name: 'Ruby',
    description: 'Rails, Sinatra',
    category: 'backend',
    packageName: 'sentry-ruby',
    installCommand: 'bundle add sentry-ruby sentry-rails',
    language: 'text',
    envVarName: 'SENTRY_DSN',
    dsnExpression: 'ENV["SENTRY_DSN"]',
    icon: <MonoLetterIcon letter="Rb" />,
    buildSnippet: (dsnExpr) => `# config/initializers/sentry.rb
require "sentry-ruby"
require "sentry-rails"

Sentry.init do |config|
  config.dsn = ${dsnExpr}
  config.environment = ENV.fetch("RAILS_ENV", "development")
  config.traces_sample_rate = 1.0
end`,
  },
  {
    id: 'java',
    name: 'Java',
    description: 'Spring Boot, Jakarta EE',
    category: 'backend',
    packageName: 'io.sentry:sentry-spring-boot-starter',
    installCommand: '# Add to pom.xml or build.gradle\nio.sentry:sentry-spring-boot-starter-jakarta:7.14.0',
    language: 'text',
    envVarName: 'SENTRY_DSN',
    dsnExpression: '${SENTRY_DSN}',
    icon: <MonoLetterIcon letter="J" />,
    buildSnippet: (dsnExpr) => `# application.properties
sentry.dsn=${dsnExpr}
sentry.environment=\${ENV:development}
sentry.traces-sample-rate=1.0`,
  },
  {
    id: 'php',
    name: 'PHP',
    description: 'Laravel, Symfony',
    category: 'backend',
    packageName: 'sentry/sentry',
    installCommand: 'composer require sentry/sentry',
    language: 'text',
    envVarName: 'SENTRY_DSN',
    dsnExpression: '$_ENV[\'SENTRY_DSN\']',
    icon: <MonoLetterIcon letter="Ph" />,
    buildSnippet: (dsnExpr) => `<?php
\\Sentry\\init([
    'dsn' => ${dsnExpr},
    'environment' => $_ENV['APP_ENV'] ?? 'development',
    'traces_sample_rate' => 1.0,
]);`,
  },
  {
    id: 'dotnet',
    name: '.NET',
    description: 'ASP.NET Core',
    category: 'backend',
    packageName: 'Sentry.AspNetCore',
    installCommand: 'dotnet add package Sentry.AspNetCore',
    language: 'text',
    envVarName: 'SENTRY_DSN',
    dsnExpression: 'Environment.GetEnvironmentVariable("SENTRY_DSN")',
    icon: <MonoLetterIcon letter=".N" />,
    buildSnippet: (dsnExpr) => `// Program.cs
builder.WebHost.UseSentry(options =>
{
    options.Dsn = ${dsnExpr};
    options.Environment = builder.Environment.EnvironmentName;
    options.TracesSampleRate = 1.0;
});`,
  },
  {
    id: 'reactnative',
    name: 'React Native',
    description: 'iOS + Android',
    category: 'mobile',
    packageName: '@sentry/react-native',
    installCommand: 'npx @sentry/wizard@latest -s -i reactNative',
    language: 'javascript',
    envVarName: 'SENTRY_DSN',
    dsnExpression: 'process.env.SENTRY_DSN',
    sentrySkill: 'sentry-react-native-sdk',
    icon: <MonoLetterIcon letter="RN" />,
    buildSnippet: (dsnExpr) => `import * as Sentry from "@sentry/react-native";

Sentry.init({
  dsn: ${dsnExpr},
  tracesSampleRate: 1.0,
  replaysSessionSampleRate: 0.1,
  replaysOnErrorSampleRate: 1.0,
});

export default Sentry.wrap(App);`,
  },
  {
    id: 'flutter',
    name: 'Flutter',
    description: 'iOS + Android + Web',
    category: 'mobile',
    packageName: 'sentry_flutter',
    installCommand: 'flutter pub add sentry_flutter',
    language: 'text',
    envVarName: 'SENTRY_DSN',
    dsnExpression: 'const String.fromEnvironment("SENTRY_DSN")',
    icon: <MonoLetterIcon letter="Fl" />,
    buildSnippet: (dsnExpr) => `import 'package:flutter/widgets.dart';
import 'package:sentry_flutter/sentry_flutter.dart';

Future<void> main() async {
  await SentryFlutter.init(
    (options) {
      options.dsn = ${dsnExpr};
      options.tracesSampleRate = 1.0;
    },
    appRunner: () => runApp(const MyApp()),
  );
}`,
  },
]

export function ErrorTrackingSetup({ project }: ErrorTrackingSetupProps) {
  const navigate = useNavigate()
  const [wizardStep, setWizardStep] = useState<WizardStepId>('framework')
  const recommendedPlatform = useMemo(
    () => platformForPreset(project.preset),
    [project.preset]
  )
  const [selectedPlatform, setSelectedPlatform] =
    useState<PlatformId>(recommendedPlatform)
  const [celebrate, setCelebrate] = useState(false)

  const { data: existingDsns, refetch: refetchDsns } = useQuery({
    ...listDsnsOptions({ path: { project_id: project.id } }),
    enabled: !!project.id,
  })

  const createDsn = useMutation({
    ...getOrCreateDsnMutation(),
    meta: { errorTitle: 'Failed to create DSN' },
  })

  const dsn = useMemo(() => {
    return existingDsns?.[0]?.dsn || 'YOUR_DSN_HERE'
  }, [existingDsns])

  const hasDsn = Boolean(existingDsns?.[0]?.dsn)

  useEffect(() => {
    if (
      wizardStep === 'install' &&
      !hasDsn &&
      !createDsn.isPending &&
      !createDsn.isSuccess &&
      !createDsn.isError
    ) {
      createDsn.mutate(
        {
          path: { project_id: project.id },
          body: {},
        },
        {
          onSuccess: () => {
            refetchDsns()
          },
        }
      )
    }
  }, [wizardStep, hasDsn, createDsn, project.id, refetchDsns])

  const { data: hasErrorsData } = useQuery({
    ...hasErrorGroupsOptions({ path: { project_id: project.id } }),
    enabled: !!project.id && wizardStep === 'waiting',
    refetchInterval: wizardStep === 'waiting' ? 2000 : false,
    refetchOnWindowFocus: false,
  })

  useEffect(() => {
    if (
      wizardStep === 'waiting' &&
      hasErrorsData?.has_error_groups &&
      !celebrate
    ) {
      setCelebrate(true)
      const timer = setTimeout(() => {
        navigate(`/projects/${project.slug}/errors`)
      }, 1600)
      return () => clearTimeout(timer)
    }
  }, [
    wizardStep,
    hasErrorsData?.has_error_groups,
    celebrate,
    navigate,
    project.slug,
  ])

  const platform = PLATFORMS.find((p) => p.id === selectedPlatform)!

  const aiPrompt = useMemo(() => {
    const dsnValue = hasDsn ? dsn : '<your-temps-dsn>'
    return `Add Temps error tracking (Sentry-compatible) to my ${platform.name} app.

## DSN environment variable

Temps automatically injects \`${platform.envVarName}\` into the runtime environment when the app is deployed on Temps — do NOT hardcode the DSN in source.

For **local development only**, add this to \`.env\`:

\`\`\`
${platform.envVarName}=${dsnValue}
\`\`\`

## Install the SDK

\`\`\`bash
${platform.installCommand}
\`\`\`

## Initialize (reads from env var)

\`\`\`${platform.language}
${platform.buildSnippet(platform.dsnExpression)}
\`\`\`

## Verify

1. Throw a test error from your app (e.g. \`throw new Error('Temps test')\`).
2. Run the app and trigger the error path.
3. Open Error Tracking → Error Groups in the Temps dashboard — the error should appear within a few seconds.
`
  }, [platform, dsn, hasDsn])

  const steps = [
    { id: 'framework' as WizardStepId, label: 'Platform' },
    { id: 'install' as WizardStepId, label: 'Install' },
    { id: 'waiting' as WizardStepId, label: 'Verify' },
  ]

  return (
    <SetupWizardShell
      title="Install error tracking"
      description="Pick your platform, drop in the Sentry-compatible SDK, and we'll wait for your first exception."
      currentStep={wizardStep}
      steps={steps}
      celebrate={celebrate}
    >
      {wizardStep === 'framework' && (
        <div className="space-y-6">
          {project.preset && PRESET_TO_PLATFORM[project.preset] && (
            <div className="flex items-start gap-3 rounded-lg border border-primary/30 bg-primary/5 p-3 text-sm">
              <Sparkles className="mt-0.5 size-4 shrink-0 text-primary" />
              <div className="min-w-0">
                <p className="font-medium">
                  Recommended for this project:{' '}
                  {PLATFORMS.find((p) => p.id === recommendedPlatform)?.name}
                </p>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  Based on the{' '}
                  <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                    {project.preset}
                  </code>{' '}
                  preset we detected in this project.
                </p>
              </div>
            </div>
          )}

          {CATEGORY_ORDER.map((cat) => {
            const items = PLATFORMS.filter((p) => p.category === cat)
            if (items.length === 0) return null
            const meta = CATEGORY_META[cat]
            return (
              <div key={cat} className="space-y-3">
                <div
                  className={cn(
                    'border-l-4 pl-3 py-0.5',
                    meta.accent
                  )}
                >
                  <div className="flex items-center gap-2">
                    <h3 className="text-sm font-semibold">{meta.label}</h3>
                    <span
                      className={cn(
                        'rounded-full px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide',
                        meta.badgeClass
                      )}
                    >
                      {items.length}
                    </span>
                  </div>
                  <p className="text-xs text-muted-foreground">{meta.hint}</p>
                </div>
                <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
                  {items.map((p) => {
                    const isSelected = selectedPlatform === p.id
                    const isRecommended = p.id === recommendedPlatform
                    return (
                      <button
                        key={p.id}
                        type="button"
                        onClick={() => setSelectedPlatform(p.id)}
                        className={cn(
                          'relative flex items-center gap-3 rounded-lg border bg-card p-4 text-left transition-all hover:border-primary/60 hover:bg-accent/40',
                          isSelected &&
                            'border-primary bg-primary/5 ring-2 ring-primary/20'
                        )}
                        aria-pressed={isSelected}
                      >
                        <div className="rounded-md bg-muted p-2 text-foreground">
                          {p.icon}
                        </div>
                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-1.5 flex-wrap">
                            <p className="font-medium leading-none">
                              {p.name}
                            </p>
                            {isRecommended &&
                              project.preset &&
                              PRESET_TO_PLATFORM[project.preset] && (
                                <span className="rounded-full bg-primary/10 px-1.5 py-0.5 text-[9px] font-medium uppercase tracking-wide text-primary">
                                  Recommended
                                </span>
                              )}
                          </div>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {p.description}
                          </p>
                        </div>
                        {isSelected && (
                          <Check className="size-4 shrink-0 text-primary" />
                        )}
                      </button>
                    )
                  })}
                </div>
              </div>
            )
          })}

          <div className="flex justify-end">
            <Button onClick={() => setWizardStep('install')}>
              Continue
              <ArrowRight className="ml-2 size-4" />
            </Button>
          </div>
        </div>
      )}

      {wizardStep === 'install' && (
        <div className="space-y-6">
          <div className="flex items-center justify-between gap-3 rounded-lg border bg-card p-4">
            <div className="flex items-center gap-3 min-w-0">
              <div className="rounded-md bg-muted p-2">{platform.icon}</div>
              <div className="min-w-0">
                <p className="font-medium leading-none">{platform.name}</p>
                <p className="mt-1 text-xs text-muted-foreground">
                  {platform.description}
                </p>
              </div>
            </div>
            <CopyButton
              value={aiPrompt}
              className="shrink-0 rounded-md border border-border px-3 py-1.5 text-xs font-medium"
            >
              Copy AI prompt
            </CopyButton>
          </div>

          {createDsn.isPending && !hasDsn && (
            <div className="flex items-center gap-2 rounded-lg border bg-muted/40 p-3 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" />
              Creating your DSN…
            </div>
          )}

          <div className="rounded-lg border border-primary/30 bg-primary/5 p-4">
            <div className="flex items-start gap-3">
              <Terminal className="mt-0.5 size-4 shrink-0 text-primary" />
              <div className="min-w-0 space-y-2">
                {platform.sentrySkill ? (
                  <>
                    <div>
                      <p className="text-sm font-medium">
                        Using an AI coding CLI? Run the official Sentry skill.
                      </p>
                      <p className="mt-1 text-xs text-muted-foreground">
                        Temps is Sentry wire-compatible, so the{' '}
                        <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                          {platform.sentrySkill}
                        </code>{' '}
                        skill from{' '}
                        <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                          getsentry/sentry-for-ai
                        </code>{' '}
                        works against your Temps DSN — just set{' '}
                        <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                          {platform.envVarName}
                        </code>{' '}
                        to the Temps DSN instead of a Sentry one.
                      </p>
                    </div>
                    <div className="space-y-1.5">
                      <p className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
                        Invoke it in your CLI
                      </p>
                      <CodeBlock
                        language="bash"
                        code={`/${platform.sentrySkill}`}
                        showCopy
                      />
                    </div>
                  </>
                ) : (
                  <>
                    <div>
                      <p className="text-sm font-medium">
                        Using an AI coding CLI? Run the Temps skill.
                      </p>
                      <p className="mt-1 text-xs text-muted-foreground">
                        Works with Claude Code, OpenCode, Codex, and any CLI
                        that supports skills. The{' '}
                        <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                          add-error-tracking
                        </code>{' '}
                        skill detects your platform and wires the
                        Sentry-compatible SDK using your Temps DSN env var.
                      </p>
                    </div>
                    <div className="space-y-1.5">
                      <p className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
                        1. Install the skill (one-time)
                      </p>
                      <CodeBlock
                        language="bash"
                        code={`npx skills add https://github.com/gotempsh/temps --skill add-error-tracking`}
                        showCopy
                      />
                    </div>
                    <div className="space-y-1.5">
                      <p className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
                        2. Invoke it in your CLI
                      </p>
                      <CodeBlock
                        language="bash"
                        code={`/add-error-tracking`}
                        showCopy
                      />
                    </div>
                  </>
                )}
              </div>
            </div>
          </div>

          <div className="space-y-3">
            <div className="flex items-center gap-2">
              <Terminal className="size-4 text-muted-foreground" />
              <h3 className="text-sm font-medium">1. Install the SDK</h3>
            </div>
            <CodeBlock
              language="bash"
              code={platform.installCommand}
              showCopy
            />
          </div>

          <div className="flex items-start gap-3 rounded-lg border bg-muted/40 p-3 text-sm">
            <Info className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
            <div className="min-w-0 space-y-1">
              <p className="font-medium">
                Temps injects{' '}
                <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                  {platform.envVarName}
                </code>{' '}
                automatically when your app deploys.
              </p>
              <p className="text-xs text-muted-foreground">
                No manual <code>.env</code> changes needed in production — the
                snippet below reads it from the environment at runtime.
              </p>
              <details className="pt-1 text-xs">
                <summary className="cursor-pointer text-muted-foreground hover:text-foreground">
                  Running locally? Set it in your <code>.env</code> for dev.
                </summary>
                <div className="mt-2">
                  <CodeBlock
                    language="bash"
                    code={`# .env (local dev only)\n${platform.envVarName}=${hasDsn ? dsn : '<your-temps-dsn>'}`}
                    showCopy
                  />
                </div>
              </details>
            </div>
          </div>

          <div className="space-y-3">
            <div className="flex items-center gap-2">
              <FileCode className="size-4 text-muted-foreground" />
              <h3 className="text-sm font-medium">2. Initialize the SDK</h3>
            </div>
            <CodeBlock
              language={platform.language}
              code={platform.buildSnippet(platform.dsnExpression)}
              showCopy
            />
          </div>

          <div className="flex items-center justify-between gap-3 pt-2">
            <Button
              variant="ghost"
              onClick={() => setWizardStep('framework')}
            >
              <ArrowLeft className="mr-2 size-4" />
              Back
            </Button>
            <Button
              onClick={() => setWizardStep('waiting')}
              disabled={!hasDsn && createDsn.isPending}
            >
              I've installed it — start listening
              <ArrowRight className="ml-2 size-4" />
            </Button>
          </div>
        </div>
      )}

      {wizardStep === 'waiting' && (
        <div className="space-y-6">
          <div className="flex flex-col items-center justify-center gap-4 rounded-xl border bg-card px-6 py-12 text-center">
            {hasErrorsData?.has_error_groups ? (
              <>
                <div className="flex size-14 items-center justify-center rounded-full bg-emerald-500/10">
                  <Check
                    className="size-7 text-emerald-500"
                    strokeWidth={3}
                  />
                </div>
                <div className="space-y-1">
                  <h3 className="text-lg font-semibold">
                    First exception received
                  </h3>
                  <p className="text-sm text-muted-foreground">
                    Taking you to your error tracking dashboard…
                  </p>
                </div>
              </>
            ) : (
              <>
                <div className="relative flex size-14 items-center justify-center">
                  <span className="absolute inline-flex size-full animate-ping rounded-full bg-primary/20" />
                  <span className="absolute inline-flex size-10 animate-ping rounded-full bg-primary/30 [animation-delay:200ms]" />
                  <span className="relative inline-flex size-4 rounded-full bg-primary" />
                </div>
                <div className="space-y-1">
                  <h3 className="text-lg font-semibold">
                    Waiting for your first exception…
                  </h3>
                  <p className="text-sm text-muted-foreground">
                    Deploy or run your app, then throw a test error. We'll
                    auto-redirect as soon as one arrives.
                  </p>
                </div>
                <div className="flex items-center gap-2 text-xs text-muted-foreground">
                  <Loader2 className="size-3 animate-spin" />
                  Polling every 2s
                </div>
              </>
            )}
          </div>

          {!hasErrorsData?.has_error_groups && (
            <details className="rounded-lg border bg-card p-4 text-sm">
              <summary className="cursor-pointer font-medium">
                Need a quick test? Throw an error from your app.
              </summary>
              <div className="mt-3 space-y-3 text-muted-foreground">
                <p>Paste this anywhere in your app to trigger a test event:</p>
                <CodeBlock
                  language={platform.language}
                  code={
                    platform.id === 'python'
                      ? `sentry_sdk.capture_message("Test from Temps setup")`
                      : `Sentry.captureMessage("Test from Temps setup");`
                  }
                  showCopy
                />
              </div>
            </details>
          )}

          <div className="flex items-center justify-between gap-3">
            <Button
              variant="ghost"
              onClick={() => setWizardStep('install')}
              disabled={celebrate}
            >
              <ArrowLeft className="mr-2 size-4" />
              Back to instructions
            </Button>
            <Button
              variant="outline"
              onClick={() => navigate(`/projects/${project.slug}/errors`)}
            >
              Skip to errors
            </Button>
          </div>
        </div>
      )}
    </SetupWizardShell>
  )
}
