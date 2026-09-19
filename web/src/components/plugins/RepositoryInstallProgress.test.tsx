// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { RepositoryInstallProgress } from './RepositoryInstallProgress'

test('does not claim stages before the host reports them', () => {
  const html = renderToStaticMarkup(
    <RepositoryInstallProgress waitingSeconds={12} />
  )
  expect(html).toContain('Waiting for the host')
  expect(html).toContain('12s')
  expect(html).not.toContain('Compiling')
})

test('retains completed stages and failure information without marking later stages done', () => {
  const html = renderToStaticMarkup(
    <RepositoryInstallProgress
      waitingSeconds={0}
      failure="Dependency download timed out. Retry after checking host connectivity."
      progress={{
        id: 'fixture',
        status: 'failed',
        elapsed_ms: 61000,
        stages: [
          {
            stage: 'fetch',
            message: 'Fetching sources',
            status: 'completed',
            elapsed_ms: 1000,
          },
          {
            stage: 'dependencies',
            message: 'Installing dependencies',
            status: 'failed',
            elapsed_ms: 60000,
          },
        ],
      }}
    />
  )
  expect(html).toContain('Fetching sources')
  expect(html).toContain('Installing dependencies')
  expect(html).toContain('Installation failed')
  expect(html).toContain('1m 1s')
  expect(html).toContain('Dependency download timed out')
  expect(html).not.toContain('Plugin installed')
})

test('successful install stops the last spinner while final progress catches up', () => {
  const html = renderToStaticMarkup(
    <RepositoryInstallProgress
      waitingSeconds={5}
      complete
      progress={{
        id: 'fixture',
        status: 'running',
        elapsed_ms: 5000,
        stages: [
          {
            stage: 'starting_plugin',
            message: 'Starting plugin',
            status: 'running',
            elapsed_ms: 1000,
          },
        ],
      }}
    />
  )
  expect(html).toContain('Plugin installed')
  expect(html).not.toContain('animate-spin')
  expect(html).not.toContain('first build')
})

test('queued installations show an active wait after source fetching completes', () => {
  const html = renderToStaticMarkup(
    <RepositoryInstallProgress
      waitingSeconds={0}
      progress={{
        id: 'queued',
        status: 'running',
        elapsed_ms: 32000,
        stages: [
          {
            stage: 'fetching_source',
            message: 'Fetching repository source',
            status: 'completed',
            elapsed_ms: 2000,
          },
          {
            stage: 'waiting_for_lifecycle',
            message: 'Waiting for plugin operations',
            status: 'running',
            elapsed_ms: 30000,
          },
        ],
      }}
    />
  )
  expect(html).toContain('Waiting for plugin operations')
  expect(html).toContain('animate-spin')
  expect(html).toContain('Installing plugin')
  expect(html).not.toContain('Plugin installed')
})
