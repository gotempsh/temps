// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import {
  ArrowRight,
  Container,
  GitBranch,
  LayoutTemplate,
  type LucideIcon,
} from 'lucide-react'
import { Link } from 'react-router'

interface DeployPathChoice {
  key: 'git' | 'image' | 'template'
  title: string
  description: string
  example: string
  href: string
  icon: LucideIcon
}

/** The three ways to deploy your own app, each opening the matching flow. */
const DEPLOY_PATH_CHOICES: readonly DeployPathChoice[] = [
  {
    key: 'git',
    title: 'Deploy from Git',
    description:
      'Connect GitHub or GitLab, or paste a public repository URL. Temps detects the framework and redeploys on every push.',
    example: 'A Next.js, Vite, Go, Python or Dockerfile repository',
    href: '/projects/new?source=browse',
    icon: GitBranch,
  },
  {
    key: 'image',
    title: 'Deploy a Docker image',
    description:
      'Run any prebuilt image from a public or private registry. No build step, no repository needed.',
    example: 'ghcr.io/you/api:1.4.0 or nginxinc/nginx-unprivileged:alpine',
    href: '/projects/new?source=manual',
    icon: Container,
  },
  {
    key: 'template',
    title: 'Start from a template',
    description:
      'Pick a ready-made starter, optionally with a database, and get a working app you can edit.',
    example: 'A starter app with Postgres attached',
    href: '/projects/new?source=templates',
    icon: LayoutTemplate,
  },
]

/**
 * One bordered choice group of deployment paths. Flat rows separated by
 * dividers, not nested cards (DESIGN.md "One surface per section").
 */
export function DeployPathChoices({
  choices = DEPLOY_PATH_CHOICES,
  className,
}: {
  choices?: readonly DeployPathChoice[]
  className?: string
}) {
  return (
    <nav
      aria-label="Deploy your own app"
      className={cn(
        'grid divide-y rounded-lg border bg-card text-card-foreground md:grid-cols-3 md:divide-x md:divide-y-0',
        className
      )}
    >
      {choices.map((choice) => (
        <Link
          key={choice.key}
          to={choice.href}
          className="group flex min-w-0 flex-col gap-2 p-4 transition-colors first:rounded-t-lg last:rounded-b-lg hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring md:first:rounded-l-lg md:first:rounded-tr-none md:last:rounded-r-lg md:last:rounded-bl-none"
        >
          <span className="flex items-center gap-2 text-sm font-medium">
            <choice.icon
              className="size-4 shrink-0 text-muted-foreground"
              aria-hidden
            />
            {choice.title}
            <ArrowRight
              className="ml-auto size-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5 motion-reduce:transition-none"
              aria-hidden
            />
          </span>
          <span className="text-sm text-muted-foreground">
            {choice.description}
          </span>
          <span className="text-xs text-muted-foreground">
            e.g. {choice.example}
          </span>
        </Link>
      ))}
    </nav>
  )
}
