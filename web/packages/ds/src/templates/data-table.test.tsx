// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Children, isValidElement, type ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { DataTable } from './data-table'

const columns = [
  { key: 'name', header: 'Name', render: (row: { name: string }) => row.name },
  {
    key: 'secondary',
    header: 'Secondary',
    className: 'hidden md:table-cell text-right',
    render: () => 'Details',
  },
]

function findClick(node: ReactNode, label: string): (() => void) | undefined {
  for (const child of Children.toArray(node)) {
    if (!isValidElement<{ children?: ReactNode; onClick?: () => void }>(child))
      continue
    if (child.props.children === label && child.props.onClick)
      return child.props.onClick
    const found = findClick(child.props.children, label)
    if (found) return found
  }
}

describe('DataTable', () => {
  test('keeps responsive column classes on every loading cell and announces loading', () => {
    const markup = renderToStaticMarkup(
      <DataTable
        aria-label="Execution history"
        columns={columns}
        rows={[]}
        rowKey={(row) => row.name}
        isLoading
      />
    )
    expect(markup).toContain('aria-label="Execution history"')
    expect(markup).toContain('aria-busy="true"')
    expect(markup).toContain('Loading rows…')
    const cells = markup.match(/<td[^>]*>/g) ?? []
    expect(cells).toHaveLength(10)
    expect(
      cells.filter((cell) => cell.includes('hidden md:table-cell text-right'))
    ).toHaveLength(5)
  })

  test('renders real rows without announcing loading', () => {
    const markup = renderToStaticMarkup(
      <DataTable
        columns={columns}
        rows={[{ name: 'Cleanup' }]}
        rowKey={(row) => row.name}
      />
    )
    expect(markup).toContain('Cleanup')
    expect(markup).toContain('aria-busy="false"')
    expect(markup).not.toContain('Loading rows…')
  })

  test.each([
    { page: 1, label: 'Previous', expected: [] },
    { page: 3, label: 'Next', expected: [] },
    { page: 2, label: 'Previous', expected: [1] },
    { page: 2, label: 'Next', expected: [3] },
  ])(
    'guards keyboard activation at pagination boundaries ($page, $label)',
    ({ page, label, expected }) => {
      const changes: number[] = []
      const tree = DataTable({
        columns,
        rows: [],
        rowKey: (row) => row.name,
        pagination: {
          page,
          pageCount: 3,
          onPageChange: (value) => changes.push(value),
        },
      })
      const click = findClick(tree, label)
      expect(click).toBeDefined()
      click?.()
      expect(changes).toEqual([...expected])
    }
  )
})
