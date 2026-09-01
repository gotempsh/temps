// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { ResponsivePagination } from './responsive-pagination'

describe('ResponsivePagination', () => {
  test('renders compact mobile navigation and full desktop controls', () => {
    const markup = renderToStaticMarkup(
      <ResponsivePagination
        page={3}
        pageSize={18}
        total={100}
        totalPages={6}
        pageSizeOptions={[9, 18, 36]}
        ariaLabel="Project list pagination"
        pageSizeAriaLabel="Projects per page"
        className="pt-2"
        onPageChange={() => {}}
        onPageSizeChange={() => {}}
      />
    )

    expect(markup).toContain(
      'class="grid grid-cols-[1fr_auto_1fr] items-center gap-3 sm:hidden"'
    )
    expect(markup).toContain('aria-label="Previous page"')
    expect(markup).toContain('aria-label="Page 3 of 6"')
    expect(markup).toContain('>3 / 6</span>')
    expect(markup).toContain('aria-label="Next page"')
    expect(markup).toContain(
      'class="hidden flex-col gap-3 sm:flex lg:flex-row lg:items-center lg:justify-between"'
    )
    expect(markup).toContain('Showing 37–54 of 100')
    expect(markup).toContain('aria-label="Projects per page"')
    expect(markup).toContain('aria-label="Page number"')
    expect(markup).toContain('aria-label="Go to first page"')
    expect(markup).toContain('aria-label="Go to last page"')
    expect(markup).toContain('>Go</button>')
  })
})
