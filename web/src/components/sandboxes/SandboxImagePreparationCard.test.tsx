// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type {
  HostImagePreparation,
  SandboxImagePreparationResponse,
} from '@/api/client'
import { getSandboxImagePreparationQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { SandboxImagePreparationCard } from './SandboxImagePreparationCard'

const IMAGE = 'ghcr.io/example/sandbox@sha256:0123'

function host(
  overrides: Partial<HostImagePreparation> = {}
): HostImagePreparation {
  return {
    node_id: 0,
    node_name: 'control plane',
    is_control_plane: true,
    eligible: true,
    ineligible_reason: null,
    keep_prepared: false,
    status: { image: IMAGE, state: 'not_pulled', already_present: false },
    status_error: null,
    prepared_image: null,
    stale_image: null,
    last_error: null,
    last_error_at: null,
    ...overrides,
  }
}

function render(
  data: SandboxImagePreparationResponse | undefined,
  canManage = true
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  if (data) client.setQueryData(getSandboxImagePreparationQueryKey(), data)
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <SandboxImagePreparationCard canManage={canManage} />
    </QueryClientProvider>
  )
}

describe('SandboxImagePreparationCard', () => {
  test('loads with a skeleton, not a spinner', () => {
    const html = render(undefined)
    expect(html).toContain('aria-busy="true"')
    expect(html).not.toContain('animate-spin')
  })

  test('onboards an unprepared host: says what it costs and offers the action', () => {
    const html = render({ hosts: [host()], all_ready: false })
    expect(html).toContain('Not pulled')
    expect(html).toContain(IMAGE)
    expect(html).toContain('several GB')
    expect(html).toContain('Prepare sandboxes')
  })

  test('non-admins see the status but cannot start a pull or change the policy', () => {
    const html = render({ hosts: [host()], all_ready: false }, false)
    expect(html).not.toContain('Prepare sandboxes')
    expect(html).toContain('An administrator can prepare the hosts')
    expect(html).toMatch(/role="switch"[^>]*disabled=""/)
  })

  test('shows download progress per host while it pulls', () => {
    const html = render({
      hosts: [
        host({
          keep_prepared: true,
          status: {
            image: IMAGE,
            state: 'pulling',
            detail: 'downloading',
            bytes_done: 500_000_000,
            bytes_total: 2_000_000_000,
            already_present: false,
          },
        }),
      ],
      all_ready: false,
    })
    expect(html).toContain('Pulling')
    expect(html).toContain('downloading · 500 MB of 2.0 GB')
    expect(html).toContain('role="progressbar"')
    expect(html).toMatch(/role="switch" aria-checked="true"/)
  })

  test('a failed pull shows its reason', () => {
    const html = render({
      hosts: [
        host({
          status: {
            image: IMAGE,
            state: 'failed',
            error: "pulling image 'x' failed: no space left on device",
            already_present: false,
          },
        }),
      ],
      all_ready: false,
    })
    expect(html).toContain('Failed')
    expect(html).toContain('no space left on device')
    expect(html).toContain('Prepare sandboxes')
  })

  test('every host ready: no call to action', () => {
    const html = render({
      hosts: [
        host({
          status: { image: IMAGE, state: 'ready', already_present: true },
        }),
        host({
          node_id: 4,
          node_name: 'worker-a',
          is_control_plane: false,
          eligible: false,
          ineligible_reason: 'draining',
          status: null,
        }),
      ],
      all_ready: true,
    })
    expect(html).toContain('Ready')
    expect(html).toContain('Not running sandboxes')
    expect(html).toContain('draining')
    expect(html).not.toContain('Prepare sandboxes')
  })

  test('says when an old image is kept because a sandbox still uses it', () => {
    const html = render({
      hosts: [
        host({
          status: { image: IMAGE, state: 'ready', already_present: false },
          stale_image: 'ghcr.io/example/sandbox:old',
        }),
      ],
      all_ready: true,
    })
    expect(html).toContain('ghcr.io/example/sandbox:old')
    expect(html).toContain('removed once nothing uses it')
  })
})
