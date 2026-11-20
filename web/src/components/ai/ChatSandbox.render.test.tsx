// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ChatSandboxStatus } from '@/api/client'
import { getChatSandboxQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import {
  ChatSandboxPending,
  SandboxLinkBody,
  SandboxOfferCard,
} from './ChatSandbox'

const CHAT = 'conv_1'

function render(status: ChatSandboxStatus, offer = false, fromProject = false) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(
    getChatSandboxQueryKey({ path: { public_id: CHAT } }),
    status
  )
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        {offer ? (
          <SandboxOfferCard
            conversationPublicId={CHAT}
            reason="unpack the archive."
            fromProject={fromProject}
          />
        ) : (
          <SandboxLinkBody
            conversationPublicId={CHAT}
            fromProject={fromProject}
          />
        )}
      </MemoryRouter>
    </QueryClientProvider>
  )
}

describe('sandbox control', () => {
  test('an unavailable sandbox says why instead of disappearing', () => {
    const markup = render({
      configured: false,
      reason: 'This server has no sandbox backend.',
      offered: false,
      connected_projects: [],
      connected_services: [],
    })
    expect(markup).toContain('Sandbox unavailable')
    expect(markup).toContain('This server has no sandbox backend.')
  })

  test('an unlinked chat shows what a sandbox is for and how to get one', () => {
    const markup = render({
      configured: true,
      offered: false,
      connected_projects: [],
      connected_services: [],
    })
    expect(markup).toContain('Give this chat a sandbox')
    expect(markup).toContain('Create a sandbox')
    expect(markup).toContain('unpack an uploaded archive')
  })

  test('a project chat says a new sandbox starts as a clone of the project', () => {
    const markup = render(
      {
        configured: true,
        offered: false,
        connected_projects: [],
        connected_services: [],
      },
      false,
      true
    )
    expect(markup).toContain('clone of this project')
    expect(markup).toContain('Create a sandbox')
    expect(markup).not.toContain('unpack an uploaded archive')
  })

  test('a project chat offer carries the same explanation', () => {
    const markup = render(
      {
        configured: true,
        offered: true,
        connected_projects: [],
        connected_services: [],
      },
      true,
      true
    )
    expect(markup).toContain('would like a sandbox to unpack the archive.')
    expect(markup).toContain('clone of this project')
  })

  test('before the first message the control is shown, disabled, and says why', () => {
    const markup = renderToStaticMarkup(<ChatSandboxPending />)
    expect(markup).toContain('Sandbox')
    expect(markup).toContain('disabled')
    expect(markup).toContain('Send your first message')
  })

  test('a linked chat names its sandbox and offers to unlink it', () => {
    const markup = render({
      configured: true,
      offered: true,
      connected_projects: [],
      connected_services: [],
      linked: {
        sandbox_id: 'sbx_abc',
        name: 'scratch',
        linked_at: '2026-10-17T09:30:00Z',
      },
    })
    expect(markup).toContain('Sandbox linked')
    expect(markup).toContain('scratch')
    expect(markup).toContain('Unlink')
    expect(markup).toContain('/sandboxes/sbx_abc')
  })

  test('a linked chat explains project databases before any project exists', () => {
    const markup = render({
      configured: true,
      offered: true,
      connected_projects: [],
      connected_services: [],
      linked: {
        sandbox_id: 'sbx_abc',
        name: 'scratch',
        linked_at: '2026-10-17T09:30:00Z',
      },
    })
    expect(markup).toContain('Project databases')
    expect(markup).toContain('never receives production')
  })
})

describe('sandbox offer card', () => {
  test('shows why the assistant wants a sandbox and lets the user decline', () => {
    const markup = render(
      {
        configured: true,
        offered: true,
        connected_projects: [],
        connected_services: [],
      },
      true
    )
    expect(markup).toContain('would like a sandbox to unpack the archive.')
    expect(markup).toContain('Create a sandbox')
    expect(markup).toContain('Not now')
  })

  test('collapses to a confirmation once a sandbox is linked', () => {
    const markup = render(
      {
        configured: true,
        offered: true,
        connected_projects: [],
        connected_services: [],
        linked: {
          sandbox_id: 'sbx_abc',
          name: 'scratch',
          linked_at: '2026-10-17T09:30:00Z',
        },
      },
      true
    )
    expect(markup).toContain('Sandbox linked: scratch')
    expect(markup).not.toContain('Not now')
  })
})
