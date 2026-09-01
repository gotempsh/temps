// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { ServiceCatalogPagination } from './ServiceCatalogPagination'

describe('ServiceCatalogPagination', () => {
  test('renders labeled mobile navigation and stable catalog page context', () => {
    const markup = renderToStaticMarkup(
      <ServiceCatalogPagination
        page={2}
        total={57}
        totalPages={3}
        onPageChange={() => {}}
      />
    )

    expect(markup).toContain('aria-label="Service catalog pagination"')
    expect(markup).toContain('aria-label="Previous page"')
    expect(markup).toContain('>Previous</button>')
    expect(markup).toContain('aria-label="Page 2 of 3"')
    expect(markup).toContain('>2 / 3</span>')
    expect(markup).toContain('aria-label="Next page"')
    expect(markup).toContain('>Next<')
    expect(markup).toContain('Showing 25–48 of 57')
  })
})
