// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { renderToStaticMarkup } from 'react-dom/server'
import type { WorkflowRunResponse } from '@/api/client'
import type { WorkflowStepResponse, WorkflowStepStatus } from '@/api/client'
import { RunFailure } from './RunFailure'
import {
  PAYLOAD_PREVIEW_CHARS,
  PAYLOAD_PREVIEW_LINES,
  failureHint,
  failureTitle,
  payloadPreview,
  payloadQueryKey,
  payloadText,
  runPayloadsKey,
  failedStepPages,
  findFailedStep,
} from './run-failure'

installBrowserWindow()
afterEach(cleanup)

function seeded() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function panel(
  client: QueryClient,
  failed: WorkflowRunResponse,
  failedStep?: WorkflowStepResponse
) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <RunFailure
        path={{ project_id: 1, environment_id: 2, run_id: 'synthetic-run' }}
        run={failed}
        failedStep={failedStep}
        invalidate={[]}
      />
    </QueryClientProvider>
  )
}

const run = {
  id: 'synthetic-run',
  project_id: 1,
  environment_id: 2,
  workflow: 'onboarding',
  status: 'failed',
  error_kind: 'workflow_failed',
  step_count: 0,
  has_error: true,
  has_input: true,
} as WorkflowRunResponse

describe('a failed run says what failed', () => {
  test('a workflow that threw before any step points at its input', () => {
    expect(failureTitle(run)).toBe('The workflow failed before its first step')
    expect(failureHint(run)).toContain('input')
    expect(failureTitle({ ...run, step_count: 2 })).toBe(
      'The workflow threw an error outside its steps'
    )
  })
  test('a failed step is named with its position and attempts', () => {
    expect(
      failureTitle(
        { error_kind: 'step_failed', step_count: 3 },
        { sequence: 1, name: 'send-welcome', attempts: 4 }
      )
    ).toBe('Step 2, send-welcome, failed after 4 attempts')
    expect(
      failureTitle(
        { error_kind: 'step_failed', step_count: 1 },
        { sequence: 0, name: 'charge', attempts: 1 }
      )
    ).toBe('Step 1, charge, failed after 1 attempt')
  })
  test('every error kind has words, and unknown ones still read as failed', () => {
    for (const kind of ['incompatible', 'payload_too_large', 'cancelled']) {
      expect(failureTitle({ error_kind: kind, step_count: 1 })).not.toBe(
        'The run failed'
      )
    }
    expect(failureTitle({ error_kind: 'something_new', step_count: 1 })).toBe(
      'The run failed'
    )
  })
  test('the error and the input show without a click, under their names', () => {
    const client = seeded()
    client.setQueryData(payloadQueryKey(run, 'error'), {
      target: 'error',
      value: 'Invalid input: email is required',
    })
    client.setQueryData(payloadQueryKey(run, 'input'), {
      target: 'input',
      value: { userId: 'u_1' },
    })
    const html = panel(client, run)
    expect(html).toContain('The workflow failed before its first step')
    expect(html).toContain('>Error</h3>')
    expect(html).toContain('Invalid input: email is required')
    expect(html).toContain('>Input</h3>')
    expect(html).toContain('&quot;userId&quot;: &quot;u_1&quot;')
    expect(html).toContain('Copy error')
    expect(html).toContain('Retry run')
    expect(html).not.toContain('Show error message')
    expect(html).not.toContain('Show all')
  })
  test('a failed step shows its own error, not the run error', () => {
    const client = seeded()
    client.setQueryData(payloadQueryKey(run, 'step_error', 1), {
      target: 'step_error',
      sequence: 1,
      value: 'SMTP refused the message',
    })
    const html = panel(
      client,
      { ...run, error_kind: 'step_failed', step_count: 2, has_input: false },
      {
        sequence: 1,
        name: 'send-welcome',
        attempts: 3,
        has_error: true,
      } as WorkflowStepResponse
    )
    expect(html).toContain('Step 2, send-welcome, failed after 3 attempts')
    expect(html).toContain('SMTP refused the message')
    expect(html).not.toContain('>Input</h3>')
  })
  test('a long payload is trimmed with a way to show all of it', () => {
    const client = seeded()
    const long = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join(
      '\n'
    )
    client.setQueryData(payloadQueryKey(run, 'error'), {
      target: 'error',
      value: long,
    })
    const html = panel(client, { ...run, has_input: false })
    expect(html).toContain('line 8')
    expect(html).not.toContain('line 9')
    expect(html).toContain('Show all (30 lines)')
  })
  test('without payload access the panel says which permission is missing', () => {
    const client = seeded()
    const query = client.getQueryCache().build<unknown, object>(client, {
      queryKey: payloadQueryKey(run, 'error'),
    })
    query.setState({
      status: 'error',
      fetchStatus: 'idle',
      error: { status: 403, title: 'Forbidden' },
      data: undefined,
    })
    const html = panel(client, { ...run, has_input: false })
    expect(html).toContain('workflow payload read permission')
  })
  test('a payload that failed to load can be read again in place', async () => {
    const client = seeded()
    const query = client.getQueryCache().build<unknown, object>(client, {
      queryKey: payloadQueryKey(run, 'error'),
    })
    query.setState({
      status: 'error',
      fetchStatus: 'idle',
      error: { status: 503, detail: 'The vault is unavailable' },
      data: undefined,
    })
    const api = mockApi({
      'POST /projects/1/environments/2/workflows/runs/synthetic-run/reveal':
        () =>
          jsonResponse({ target: 'error', value: 'SMTP refused the message' }),
    })
    try {
      const view = render(
        <QueryClientProvider client={client}>
          <RunFailure
            path={{ project_id: 1, environment_id: 2, run_id: 'synthetic-run' }}
            run={{ ...run, has_input: false }}
            invalidate={[]}
          />
        </QueryClientProvider>
      )
      expect(view.getByText('The vault is unavailable')).toBeTruthy()
      fireEvent.click(view.getByRole('button', { name: 'Try again' }))
      expect(await view.findByText('SMTP refused the message')).toBeTruthy()
      expect(api.calls).toHaveLength(1)
    } finally {
      api.restore()
    }
  })
  test('a missing permission offers no retry, which could not fix it', () => {
    const client = seeded()
    const query = client.getQueryCache().build<unknown, object>(client, {
      queryKey: payloadQueryKey(run, 'error'),
    })
    query.setState({
      status: 'error',
      fetchStatus: 'idle',
      error: { status: 403, title: 'Forbidden' },
      data: undefined,
    })
    const html = panel(client, { ...run, has_input: false })
    expect(html).not.toContain('Try again')
  })
  test('a run without stored error or input offers only the retry', () => {
    const html = panel(seeded(), { ...run, has_error: false, has_input: false })
    expect(html).not.toContain('>Error</h3>')
    expect(html).not.toContain('>Input</h3>')
    expect(html).toContain('Retry run')
  })
  test('long payloads trim by lines or by characters, short ones stay whole', () => {
    expect(payloadPreview('short')).toBeNull()
    const lines = Array.from({ length: PAYLOAD_PREVIEW_LINES + 2 }, () => 'x')
    expect(payloadPreview(lines.join('\n'))).toBe(
      `${lines.slice(0, PAYLOAD_PREVIEW_LINES).join('\n')}…`
    )
    const wide = 'y'.repeat(PAYLOAD_PREVIEW_CHARS + 50)
    expect(payloadPreview(wide)).toBe(`${'y'.repeat(PAYLOAD_PREVIEW_CHARS)}…`)
  })
  test('revealed payloads render as text or pretty JSON', () => {
    expect(payloadText('TypeError: email is undefined')).toBe(
      'TypeError: email is undefined'
    )
    expect(payloadText({ email: 'a@example.com' })).toBe(
      '{\n  "email": "a@example.com"\n}'
    )
    expect(payloadText('\n  Error: boom\n')).toBe('Error: boom')
    expect(payloadText(null)).toBe('Nothing was stored.')
  })
})

describe('a retried run never shows the previous failure', () => {
  test('a new attempt or finish time reads its payloads again', () => {
    const first = {
      ...run,
      attempt: 1,
      finished_at: '2026-01-01T00:00:00Z',
    } as WorkflowRunResponse
    const client = seeded()
    client.setQueryData(payloadQueryKey(first, 'error'), {
      target: 'error',
      value: 'first failure',
    })
    expect(panel(client, first)).toContain('first failure')
    for (const again of [
      { ...first, attempt: 2 },
      { ...first, finished_at: '2026-01-02T00:00:00Z' },
    ]) {
      expect(payloadQueryKey(again, 'error')).not.toEqual(
        payloadQueryKey(first, 'error')
      )
      expect(panel(client, again)).not.toContain('first failure')
    }
    // Every payload of the run shares one prefix, so a retry clears them all.
    client.removeQueries({ queryKey: runPayloadsKey(first.id) })
    expect(client.getQueryData(payloadQueryKey(first, 'error'))).toBeUndefined()
  })
})

describe('the failed step is found at the end of the journal', () => {
  test('the last two pages of 100 are read, whatever page is on screen', () => {
    expect(failedStepPages(0)).toEqual([])
    expect(failedStepPages(1)).toEqual([1])
    expect(failedStepPages(100)).toEqual([1])
    expect(failedStepPages(101)).toEqual([1, 2])
    expect(failedStepPages(250)).toEqual([2, 3])
  })
  test('the most recently recorded failed step wins', () => {
    const step = (sequence: number, status: WorkflowStepStatus) => ({
      sequence,
      status,
    })
    expect(
      findFailedStep([
        [step(98, 'failed'), step(99, 'completed')],
        [step(100, 'completed'), step(101, 'failed'), step(102, 'completed')],
      ])?.sequence
    ).toBe(101)
    expect(findFailedStep([[step(0, 'completed')]])).toBeUndefined()
    expect(findFailedStep([])).toBeUndefined()
  })
})
