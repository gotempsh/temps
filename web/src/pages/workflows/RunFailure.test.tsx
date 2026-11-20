// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import type { WorkflowRunResponse } from '@/api/client'
import type { WorkflowStepResponse } from '@/api/client'
import { RunFailure } from './RunFailure'
import {
  PAYLOAD_PREVIEW_CHARS,
  PAYLOAD_PREVIEW_LINES,
  failureHint,
  failureTitle,
  payloadPreview,
  payloadQueryKey,
  payloadText,
} from './run-failure'

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
    client.setQueryData(payloadQueryKey('synthetic-run', 'error'), {
      target: 'error',
      value: 'Invalid input: email is required',
    })
    client.setQueryData(payloadQueryKey('synthetic-run', 'input'), {
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
    client.setQueryData(payloadQueryKey('synthetic-run', 'step_error', 1), {
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
    client.setQueryData(payloadQueryKey('synthetic-run', 'error'), {
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
      queryKey: payloadQueryKey('synthetic-run', 'error'),
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
