// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
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

  test('renders complete shared pagination metadata and controls', () => {
    const markup = renderToStaticMarkup(<DataTable columns={columns} rows={[]}
      rowKey={(row) => row.name} pagination={{page: 2, pageSize: 10, total: 25,
        totalPages: 3, onPageChange: () => {}, onPageSizeChange: () => {},
        pageSizeOptions: [10, 25]}} />)
    expect(markup).toContain('Showing 11–20 of 25')
    expect(markup).toContain('aria-label="Items per page"')
    expect(markup).toContain('aria-label="Go to last page"')
    expect(markup).toContain('aria-label="Page number"')
  })
})

test('custom rows retain expansion rows inside the shared table', () => {
  const markup = renderToStaticMarkup(
    <DataTable
      columns={columns}
      rows={[{ name: 'Event' }]}
      rowKey={(row) => row.name}
      renderRow={(row) => (
        <>
          <tr>
            <td>{row.name}</td>
            <td>Actor</td>
          </tr>
          <tr>
            <td colSpan={2}>Expanded metadata</td>
          </tr>
        </>
      )}
    />
  )
  expect(markup).toContain('Expanded metadata')
  expect(markup).toContain('colSpan="2"')
  expect(markup.match(/<tr/g)).toHaveLength(3)
})
