// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client/types.gen'
import { sameUserHref } from '@/lib/visitor-identity'
import { VisitorIdentityPanel } from './VisitorIdentityPanel'

const project = { id: 3, name: 'Site', slug: 'site' } as ProjectResponse

function render(
  visitor: Parameters<typeof VisitorIdentityPanel>[0]['visitor']
): string {
  return renderToStaticMarkup(
    <MemoryRouter>
      <VisitorIdentityPanel project={project} visitor={visitor} />
    </MemoryRouter>
  )
}

describe('VisitorIdentityPanel', () => {
  test('an identified visitor shows the user id, its source and a link to other devices', () => {
    const html = render({
      user_id: 'user_123',
      identity_source: 'server',
      identified_at: '2026-11-20T10:00:00Z',
    })
    expect(html).toContain('user_123')
    expect(html).toContain('Verified by backend')
    expect(html).toContain('Same user on other devices')
    expect(html).toContain('filter_user_id=user_123')
  })

  test('a browser claim is labelled as one', () => {
    expect(
      render({ user_id: 'user_123', identity_source: 'browser' })
    ).toContain('Claimed by browser')
  })

  test('an anonymous visitor explains how to identify users and links to setup', () => {
    const html = render({})
    expect(html).toContain('Anonymous')
    expect(html).toContain('/projects/site/analytics/settings')
  })
})

describe('sameUserHref', () => {
  test('encodes the user id', () => {
    expect(sameUserHref('site', 'a b&c')).toBe(
      '/projects/site/analytics/visitors?filter_user_id=a+b%26c&time_range=90d'
    )
  })
})
