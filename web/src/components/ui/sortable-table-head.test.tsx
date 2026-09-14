// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { SortableTableHead } from './sortable-table-head'

function render(active: boolean, direction: 'asc' | 'desc' = 'desc') {
  return renderToStaticMarkup(
    <table>
      <thead>
        <tr>
          <SortableTableHead
            label="Duration"
            active={active}
            direction={direction}
            onClick={() => undefined}
            align="right"
            className="custom-head"
          />
        </tr>
      </thead>
    </table>
  )
}

describe('SortableTableHead', () => {
  test('renders an accessible inactive sort control', () => {
    const html = render(false)

    expect(html).toContain('<th class="')
    expect(html).toContain('scope="col"')
    expect(html).not.toContain('aria-sort=')
    expect(html).toContain('type="button"')
    expect(html).toContain('aria-label="Sort by Duration, descending"')
    expect(html).toContain('custom-head')
  })

  test('reports ascending state and offers descending sorting', () => {
    const html = render(true, 'asc')

    expect(html).toContain('aria-sort="ascending"')
    expect(html).toContain('aria-label="Sort by Duration, descending"')
  })

  test('reports descending state and offers ascending sorting', () => {
    const html = render(true, 'desc')

    expect(html).toContain('aria-sort="descending"')
    expect(html).toContain('aria-label="Sort by Duration, ascending"')
  })
})
