// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ActivityStatus } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { ActivityRequestState } from './ActivityRequestState'
import { useActivityGate } from './use-activity-gate'

installBrowserWindow()
afterEach(cleanup)

type GateStatus = Pick<ActivityStatus, 'in_progress' | 'cooldowns'>

const idle: GateStatus = {
  in_progress: null,
  cooldowns: {
    website_analysis_secs: null,
    preview_secs: null,
    report_secs: null,
  },
}

function Harness({
  status,
  statusUpdatedAt,
  error = null,
  isPending = false,
  onRetry,
}: {
  status: GateStatus
  statusUpdatedAt: number
  error?: unknown
  isPending?: boolean
  onRetry?: () => void
}) {
  const gate = useActivityGate(status, 'website_analysis', statusUpdatedAt)
  return (
    <MemoryRouter>
      <button type="button" disabled={gate.blocked}>
        Analyze website
      </button>
      <ActivityRequestState
        operation="website_analysis"
        gate={gate}
        isPending={isPending}
        error={error}
        onRetry={onRetry}
      />
    </MemoryRouter>
  )
}

test('a running analysis shows its live elapsed time and blocks the button', async () => {
  const startedAt = new Date(Date.now() - 12_000).toISOString()
  render(
    <Harness
      statusUpdatedAt={Date.now()}
      status={{
        ...idle,
        in_progress: { operation: 'website_analysis', started_at: startedAt },
      }}
      // Even a stale "busy" response is shown as progress, not as a failure.
      error={{ code: 'analysis_running', detail: 'already running' }}
    />
  )
  expect(screen.getByRole('status').textContent).toMatch(
    /Website analysis is running for this project · started 1[23]s ago/
  )
  expect(screen.queryByText('already running')).toBeNull()
  expect(
    (
      screen.getByRole('button', {
        name: 'Analyze website',
      }) as HTMLButtonElement
    ).disabled
  ).toBe(true)
  await act(() => new Promise((resolve) => setTimeout(resolve, 1100)))
  expect(screen.getByRole('status').textContent).toMatch(/started 1[34]s ago/)
})

test('a cooldown disables the button and counts down until it re-enables', async () => {
  const realNow = Date.now
  let now = 1_700_000_000_000
  Date.now = () => now
  try {
    render(
      <Harness
        status={{
          ...idle,
          cooldowns: { ...idle.cooldowns, website_analysis_secs: 2 },
        }}
        statusUpdatedAt={now}
        error={{ code: 'cooling_down', detail: 'Try again in 2s.' }}
      />
    )
    const button = () =>
      screen.getByRole('button', {
        name: 'Analyze website',
      }) as HTMLButtonElement
    expect(screen.getByRole('status').textContent).toBe(
      'Website analysis is available again in 2s'
    )
    expect(button().disabled).toBe(true)
    now += 1000
    await act(() => new Promise((resolve) => setTimeout(resolve, 1100)))
    expect(screen.getByRole('status').textContent).toBe(
      'Website analysis is available again in 1s'
    )
    now += 1000
    await act(() => new Promise((resolve) => setTimeout(resolve, 1100)))
    expect(screen.queryByRole('status')).toBeNull()
    expect(button().disabled).toBe(false)
    // The cooldown response is not left behind as an error.
    expect(screen.queryByRole('alert')).toBeNull()
  } finally {
    Date.now = realNow
  }
})

test('a provider error shows its message, a settings link and a working retry', () => {
  const retry = mock(() => {})
  render(
    <Harness
      statusUpdatedAt={Date.now()}
      status={idle}
      error={{
        code: 'provider_error',
        provider_failure: 'model_not_found',
        model: 'mock-flash',
        setup_url: '/ai-gateway',
        detail:
          "The AI provider rejected the request: model 'mock-flash' not found — check AI Gateway settings.",
      }}
      onRetry={retry}
    />
  )
  const alert = screen.getByRole('alert')
  expect(alert.textContent).toContain("model 'mock-flash' not found")
  expect(
    screen
      .getByRole('link', { name: 'Open AI Gateway settings' })
      .getAttribute('href')
  ).toBe('/ai-gateway')
  fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
  expect(retry).toHaveBeenCalledTimes(1)
  expect(
    (
      screen.getByRole('button', {
        name: 'Analyze website',
      }) as HTMLButtonElement
    ).disabled
  ).toBe(false)
})

test('nothing is shown while the request is still pending', () => {
  render(
    <Harness
      statusUpdatedAt={Date.now()}
      status={idle}
      isPending
      error={{ code: 'provider_error', detail: 'old failure' }}
    />
  )
  expect(screen.queryByRole('alert')).toBeNull()
  expect(screen.queryByRole('status')).toBeNull()
})
