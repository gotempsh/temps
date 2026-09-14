// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { WorkspaceHarnessActivity } from '@/api/client'
import {
  groupHarnessActivity,
  WorkspaceActivity,
  WorkspaceRunningIndicator,
} from './WorkspaceActivity'

function harness(
  ai_provider: string,
  overrides: Partial<WorkspaceHarnessActivity> = {}
): WorkspaceHarnessActivity {
  return {
    ai_provider,
    total: 0,
    pending: 0,
    running: 0,
    completed: 0,
    failed: 0,
    cancelled: 0,
    idle: 0,
    ...overrides,
  }
}

describe('workspace activity', () => {
  test('moves running activity out of the details row without a visible count', () => {
    const html = renderToStaticMarkup(
      <WorkspaceActivity
        harnesses={[harness('codex_cli', { total: 1, running: 1 })]}
      />
    )
    expect(html).toContain('overflow-hidden')
    expect(html).not.toContain('overflow-x-auto')
    expect(html).not.toContain('animate-spin')
    const indicator = renderToStaticMarkup(
      <WorkspaceRunningIndicator
        harnesses={[harness('codex_cli', { running: 2 })]}
      />
    )
    expect(indicator).toContain('aria-label="Threads running"')
    expect(indicator).toContain('motion-reduce:animate-none')
    expect(indicator.replace(/<[^>]*>/g, '')).toBe('')
    expect(
      renderToStaticMarkup(<WorkspaceRunningIndicator harnesses={[]} />)
    ).toBe('')
    expect(renderToStaticMarkup(<WorkspaceRunningIndicator />)).toBe('')
  })
  test('keeps the project icon and count in the same row as harnesses and statuses', () => {
    const html = renderToStaticMarkup(
      <WorkspaceActivity
        projectCount={2}
        harnesses={[harness('codex_cli', { total: 1, completed: 1 })]}
      />
    )
    expect(html).toContain('lucide-folder')
    expect(html).toContain('aria-label="2 projects"')
    expect(html.match(/<p\b/g)).toHaveLength(1)
    expect(html.replace(/<[^>]*>/g, '').trim()).toBe('211')
    expect(html).not.toContain('flex-wrap')
  })
  test('retains the project count when activity is unavailable or archived', () => {
    expect(
      renderToStaticMarkup(<WorkspaceActivity projectCount={1} error />)
    ).toContain('aria-label="1 project"')
    const archived = renderToStaticMarkup(
      <WorkspaceActivity projectCount={0} showThreads={false} />
    )
    expect(archived).toContain('aria-label="0 projects"')
    expect(archived).not.toContain('Activity unavailable')
  })
  test('shows each harness logo and count alongside persistent terminal states', () => {
    const html = renderToStaticMarkup(
      <WorkspaceActivity
        harnesses={[
          harness('codex_cli', {
            total: 5,
            completed: 3,
            failed: 1,
            running: 1,
          }),
          harness('claude_cli', { total: 2, pending: 1, cancelled: 1 }),
          harness('opencode', { total: 1, idle: 1 }),
        ]}
      />
    )
    for (const text of [
      'Codex: 5 threads',
      'Claude Code: 2 threads',
      'OpenCode: 1 thread',
      '1 pending thread',
      '3 finished threads',
      '1 failed thread',
      '1 stopped thread',
      '1 idle thread',
    ])
      expect(html).toContain(text)
    for (const id of ['codex_cli', 'claude_cli', 'opencode'])
      expect(html).toContain(`data-harness="${id}"`)
    expect(html).not.toContain('animate-spin')
  })
  test('merges historical aliases without modifying API data', () => {
    const rows = [
      harness('codex', { total: 2, completed: 2 }),
      harness('codex_cli', { total: 1, failed: 1 }),
    ]
    expect(groupHarnessActivity(rows)).toEqual([
      harness('codex_cli', { total: 3, completed: 2, failed: 1 }),
    ])
    expect(rows[0].total).toBe(2)
  })
  test('finished and failed counts remain visible with no pending work', () => {
    const html = renderToStaticMarkup(
      <WorkspaceActivity
        harnesses={[harness('opencode', { total: 2, completed: 1, failed: 1 })]}
      />
    )
    expect(html).toContain('1 finished thread')
    expect(html).toContain('lucide-circle-check')
    expect(html).toContain('1 failed thread')
    expect(html).not.toContain('animate-spin')
    expect(html).not.toContain('Pending')
  })
  test('does not claim an empty or unavailable workspace has finished work', () => {
    const empty = renderToStaticMarkup(<WorkspaceActivity harnesses={[]} />)
    expect(empty).toContain('No threads yet')
    expect(empty).not.toContain('Finished')
    expect(renderToStaticMarkup(<WorkspaceActivity loading />)).toContain(
      'Loading workspace activity'
    )
    expect(renderToStaticMarkup(<WorkspaceActivity error />)).toContain(
      'Activity unavailable'
    )
    expect(renderToStaticMarkup(<WorkspaceActivity />)).toContain(
      'Activity unavailable'
    )
  })
  test('keeps unknown harnesses discoverable with a fallback logo', () => {
    expect(
      renderToStaticMarkup(
        <WorkspaceActivity
          harnesses={[harness('custom-runner', { total: 1, idle: 1 })]}
        />
      )
    ).toContain('custom-runner: 1 thread')
  })
  test('renders only numbers in one non-wrapping line, retaining accessible status labels', () => {
    const html = renderToStaticMarkup(
      <WorkspaceActivity
        harnesses={[
          harness('codex_cli', {
            total: 6,
            completed: 2,
            running: 1,
            pending: 1,
            failed: 1,
            cancelled: 1,
          }),
        ]}
      />
    )
    const visibleText = html.replace(/<[^>]*>/g, '').trim()
    expect(visibleText).toMatch(/^[0-9]+$/)
    expect(html).toContain('whitespace-nowrap')
    expect(html).not.toContain('flex-wrap')
    expect(html).toContain('aria-label="2 finished threads"')
    expect(html).not.toContain('aria-label="1 running thread"')
    expect(html).toContain('aria-label="1 failed thread"')
  })
})
