// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { PassThrough } from 'node:stream'
import { render } from 'ink'
import { DeploymentWatcher, formatDuration } from './deployment-watcher.js'

async function watch(response: () => Response, jobs = () => Response.json({ jobs: [] })) {
  const paths: string[] = []
  const server = Bun.serve({
    port: 0,
    hostname: '127.0.0.1',
    fetch(request) {
      const path = new URL(request.url).pathname
      paths.push(path)
      expect(request.headers.get('authorization')).toBe('Bearer test-token')
      if (path === '/api/projects/7/deployments/42') return response()
      if (path === '/api/projects/7/deployments/42/jobs') return jobs()
      return new Response('Unexpected route', { status: 404 })
    },
  })
  const output = new PassThrough()
  output.resume()
  let instance: ReturnType<typeof render> | undefined
  try {
    const result = await new Promise<{ success: boolean; error?: string }>((resolve) => {
      instance = render(<DeploymentWatcher projectId={7} deploymentId={42}
        timeoutSecs={4} apiUrl={`http://127.0.0.1:${server.port}/api`}
        apiKey="test-token" onComplete={resolve} />,
      { stdout: output as unknown as NodeJS.WriteStream, stderr: output as unknown as NodeJS.WriteStream, stdin: new PassThrough() as unknown as NodeJS.ReadStream, exitOnCtrlC: false, patchConsole: false })
    })
    return { result, paths }
  } finally {
    instance?.unmount()
    server.stop(true)
    output.destroy()
  }
}

describe('deployment watcher HTTP polling', () => {
  for (const status of [400, 401, 403, 404]) {
    test(`stops immediately on HTTP ${status}`, async () => {
      const { result, paths } = await watch(() => new Response('Cannot read deployment', { status }))
      expect(result.success).toBe(false)
      expect(result.error).toContain(`API Error ${status}: Cannot read deployment`)
      expect(paths).toEqual(['/api/projects/7/deployments/42'])
    })
  }

  test('reports a failed deployment through the project-scoped route', async () => {
    const { result, paths } = await watch(() => Response.json({ id: 42, status: 'failed', cancelled_reason: 'Bundle contains a forbidden dotfile' }))
    expect(result).toMatchObject({ success: false, error: 'Bundle contains a forbidden dotfile' })
    expect(paths).toEqual(['/api/projects/7/deployments/42', '/api/projects/7/deployments/42/jobs'])
  })

  test('job response errors cannot hide terminal deployment failure', async () => {
    const { result, paths } = await watch(
      () => Response.json({ id: 42, status: 'failed', cancelled_reason: 'Invalid bundle' }),
      () => new Response('invalid JSON'),
    )
    expect(result).toMatchObject({ success: false, error: 'Invalid bundle' })
    expect(paths).toHaveLength(2)
  })

  test('reports the failing job when the deployment has no reason', async () => {
    const { result } = await watch(
      () => Response.json({ id: 42, status: 'failed' }),
      () => Response.json({ jobs: [{ id: 1, job_id: 'validate', name: 'Validate', status: 'failure', error_message: 'Archive validation failed' }] }),
    )
    expect(result.error).toBe('Archive validation failed')
  })

  test('retries transient server errors and completes successfully', async () => {
    let attempts = 0
    const { result } = await watch(() => ++attempts === 1
      ? new Response('Temporarily unavailable', { status: 503 })
      : Response.json({ id: 42, status: 'completed' }))
    expect(result.success).toBe(true)
    expect(attempts).toBe(2)
  })

  for (const status of [408, 425, 429]) {
    test(`retries HTTP ${status} and completes successfully`, async () => {
      let attempts = 0
      const { result } = await watch(() => ++attempts === 1
        ? new Response('Please retry', { status })
        : Response.json({ id: 42, status: 'completed' }))
      expect(result.success).toBe(true)
      expect(attempts).toBe(2)
    })
  }
})

describe('formatDuration', () => {
  test('reads the ISO 8601 timestamps the API sends', () => {
    expect(formatDuration('2026-11-25T10:00:00Z', '2026-11-25T10:01:05.250Z')).toBe('1m 5s')
    expect(formatDuration('2026-11-25T10:00:00Z', null, Date.parse('2026-11-25T10:00:42Z'))).toBe('42s')
  })

  test('still reads epoch seconds and milliseconds from older servers', () => {
    expect(formatDuration(1_764_064_800, 1_764_064_865)).toBe('1m 5s')
    expect(formatDuration(1_764_064_800_000, 1_764_064_830_000)).toBe('30s')
  })

  test('an unreadable timestamp gives no duration instead of NaN', () => {
    expect(formatDuration('not a date', '2026-11-25T10:01:05Z')).toBe('')
    expect(formatDuration('2026-11-25T10:00:00Z', 'soon')).toBe('')
  })
})
