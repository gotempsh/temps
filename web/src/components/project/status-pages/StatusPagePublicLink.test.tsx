// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { PublicLinkCell, PublicLinkSection } from './StatusPagePublicLink'

function render(page: Parameters<typeof PublicLinkSection>[0]['page']) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <PublicLinkSection page={page} />
    </MemoryRouter>
  )
}

describe('PublicLinkCell', () => {
  const renderCell = (page: Parameters<typeof PublicLinkCell>[0]['page']) =>
    renderToStaticMarkup(
      <MemoryRouter>
        <PublicLinkCell page={page} />
      </MemoryRouter>
    )

  test('a draft is not public', () => {
    expect(
      renderCell({ is_published: false, public_url: 'https://x.test/a' })
    ).toContain('Not public')
  })

  test('a published page with a URL offers to copy it', () => {
    const html = renderCell({
      is_published: true,
      public_url: 'https://temps.example.com/api/status/abc',
      public_url_status: { configured: true },
    })
    expect(html).toContain('Copy link')
  })

  test('a published page without a URL links to the setting, not to a copy', () => {
    const html = renderCell({
      is_published: true,
      public_url: null,
      public_url_status: {
        configured: false,
        reason: 'Public URL is not configured.',
        setup_path: '/settings#public-ingress',
      },
    })
    expect(html).toContain('Public URL not configured')
    expect(html).toContain('href="/settings#public-ingress"')
    expect(html).not.toContain('Copy link')
  })
})

describe('PublicLinkSection', () => {
  test('offers Copy and Open for a published page with a public URL', () => {
    const html = render({
      is_published: true,
      public_url: 'https://temps.example.com/api/status/abc',
      public_url_status: { configured: true },
    })

    expect(html).toContain('https://temps.example.com/api/status/abc')
    expect(html).toContain('Copy')
    expect(html).toContain('Open')
    expect(html).not.toContain('Public URL is not configured')
  })

  test('does not offer Open while the page is a draft', () => {
    const html = render({
      is_published: false,
      public_url: 'https://temps.example.com/api/status/abc',
      public_url_status: { configured: true },
    })

    expect(html).toContain('Copy')
    expect(html).not.toContain('>Open<')
    expect(html).toContain('returns “not found” until you publish')
  })

  test('onboards instead of offering a broken link when no URL is configured', () => {
    const html = render({
      is_published: true,
      public_url: null,
      public_url_status: {
        configured: false,
        reason: 'Public URL is not configured. Set one.',
        setup_path: '/settings#public-ingress',
      },
    })

    expect(html).toContain('Public link')
    expect(html).toContain('Public URL is not configured')
    expect(html).toContain('href="/settings#public-ingress"')
    expect(html).toContain('Set the public URL')
    // No address was invented, so nothing to copy or open.
    expect(html).not.toContain('href="http')
    expect(html).not.toContain('localho.st')
    expect(html).not.toContain('>Copy<')
    expect(html).not.toContain('>Open<')
  })

  test('keeps the link but warns when the URL will not open from outside', () => {
    const html = render({
      is_published: true,
      public_url: 'http://localhost:3000/api/status/abc',
      public_url_status: {
        configured: true,
        reason: 'localhost only resolves on the machine itself',
        setup_path: '/settings#public-ingress',
      },
    })

    expect(html).toContain('http://localhost:3000/api/status/abc')
    expect(html).toContain('This link may not open for visitors')
    expect(html).toContain('href="/settings#public-ingress"')
  })
})
