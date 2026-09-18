// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Invented fixture data — plausible numbers, no real names/customers/hostnames.
import type { StatusTone } from '@temps-sdk/ds'

export interface DeploymentFixture {
  id: string
  service: string
  branch: string
  commit: string
  status: StatusTone
  statusLabel: string
  durationMs: number
  createdAt: string
  author: string
}

export const DEPLOYMENTS: DeploymentFixture[] = [
  {
    id: 'dep_9f2a',
    service: 'checkout-api',
    branch: 'main',
    commit: 'a91c3de',
    status: 'ok',
    statusLabel: 'Live',
    durationMs: 42_300,
    createdAt: '2026-09-17T08:12:00Z',
    author: 'a.rivera',
  },
  {
    id: 'dep_7b04',
    service: 'checkout-api',
    branch: 'feature/retry-webhook',
    commit: '1c88f21',
    status: 'running',
    statusLabel: 'Deploying',
    durationMs: 18_900,
    createdAt: '2026-09-17T07:41:00Z',
    author: 'm.chen',
  },
  {
    id: 'dep_5e91',
    service: 'marketing-site',
    branch: 'main',
    commit: 'f402ab7',
    status: 'error',
    statusLabel: 'Build failed',
    durationMs: 61_200,
    createdAt: '2026-09-16T22:03:00Z',
    author: 'a.rivera',
  },
  {
    id: 'dep_3a10',
    service: 'worker-pool',
    branch: 'main',
    commit: '77d0e5c',
    status: 'ok',
    statusLabel: 'Live',
    durationMs: 35_700,
    createdAt: '2026-09-16T19:18:00Z',
    author: 'j.okafor',
  },
  {
    id: 'dep_2c48',
    service: 'checkout-api',
    branch: 'main',
    commit: 'bb013aa',
    status: 'idle',
    statusLabel: 'Superseded',
    durationMs: 39_400,
    createdAt: '2026-09-15T14:52:00Z',
    author: 'm.chen',
  },
]

export const DEPLOYMENT_METRICS = Array.from({ length: 24 }, (_, i) => ({
  t: `${String(i).padStart(2, '0')}:00`,
  p50: 110 + Math.round(Math.sin(i / 3) * 20 + Math.random() * 10),
  p99: 340 + Math.round(Math.sin(i / 3) * 60 + Math.random() * 30),
}))

export interface ProjectFixture {
  id: string
  name: string
  slug: string
  status: StatusTone
  statusLabel: string
  lastDeployedAt: string
  deployCount: number
}

export const PROJECTS: ProjectFixture[] = [
  {
    id: 'proj_9f2a',
    name: 'checkout-api',
    slug: 'checkout-api',
    status: 'ok',
    statusLabel: 'Live',
    lastDeployedAt: '2026-09-17T08:12:00Z',
    deployCount: 214,
  },
  {
    id: 'proj_7b04',
    name: 'marketing-site',
    slug: 'marketing-site',
    status: 'error',
    statusLabel: 'Build failed',
    lastDeployedAt: '2026-09-16T22:03:00Z',
    deployCount: 88,
  },
  {
    id: 'proj_5e91',
    name: 'worker-pool',
    slug: 'worker-pool',
    status: 'ok',
    statusLabel: 'Live',
    lastDeployedAt: '2026-09-16T19:18:00Z',
    deployCount: 42,
  },
  {
    id: 'proj_3a10',
    name: 'internal-dashboard',
    slug: 'internal-dashboard',
    status: 'running',
    statusLabel: 'Deploying',
    lastDeployedAt: '2026-09-17T07:41:00Z',
    deployCount: 61,
  },
  {
    id: 'proj_2c48',
    name: 'docs-site',
    slug: 'docs-site',
    status: 'idle',
    statusLabel: 'Superseded',
    lastDeployedAt: '2026-09-10T14:52:00Z',
    deployCount: 19,
  },
]

export interface CronExecutionFixture {
  id: string
  path: string
  executedAt: string
  statusCode: number
  durationMs: number
  error?: string
}

export const CRON_EXECUTIONS: CronExecutionFixture[] = [
  {
    id: 'run-3',
    path: '/tasks/cleanup',
    executedAt: '2026-09-18T10:00:00Z',
    statusCode: 200,
    durationMs: 250,
  },
  {
    id: 'run-2',
    path: '/tasks/digest',
    executedAt: '2026-09-18T09:30:00Z',
    statusCode: 503,
    durationMs: 1200,
    error: 'Service unavailable',
  },
  {
    id: 'run-1',
    path: '/tasks/cleanup',
    executedAt: '2026-09-18T09:00:00Z',
    statusCode: 204,
    durationMs: 180,
  },
]

/** Deterministic sample runs across seven days, anchored when the example opens. */
export function createExecutionFixtures(now: number): CronExecutionFixture[] {
  const paths = [
    '/tasks/cleanup',
    '/tasks/digest',
    '/tasks/sync',
    '/tasks/reports',
  ]
  return Array.from({ length: 672 }, (_, index) => ({
    id: `sample-run-${index}`,
    path: paths[index % paths.length],
    executedAt: new Date(now - (index * 15 + 5) * 60_000).toISOString(),
    statusCode: index % 17 === 0 ? 503 : 200,
    durationMs: index % 17 === 0 ? 1200 : 140 + ((index * 37) % 680),
    error: index % 17 === 0 ? 'Service unavailable' : undefined,
  }))
}
