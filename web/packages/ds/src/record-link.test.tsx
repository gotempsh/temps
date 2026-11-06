// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import { TableBody, TableCell, Table } from '@temps-sdk/ui'
import { followsRecordLink } from './lib/follows-record-link'
import { RecordLink, RecordListItem, RecordRow } from './record-link'

test('record navigation is a real named link styled as the record name', () => {
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <RecordLink
        to="/variables/7"
        aria-label="View API_KEY details"
        className="font-mono"
        target="_blank"
        rel="noopener"
      >
        API_KEY
      </RecordLink>
    </MemoryRouter>
  )
  expect(markup).toContain('href="/variables/7"')
  expect(markup).toContain('aria-label="View API_KEY details"')
  expect(markup).toContain('target="_blank"')
  expect(markup).toContain('data-record-link=""')
  expect(markup).toContain('font-mono')
  expect(markup).toContain('no-underline')
  expect(markup).not.toContain('hover:underline')
  expect(markup).not.toContain('<svg')
  expect(markup).toContain('focus-visible:outline-2')
  expect(markup).not.toContain('<button')
})

test('a record row keeps table semantics and only points when it holds a link', () => {
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <Table>
        <TableBody>
          <RecordRow className="border-b">
            <TableCell>
              <RecordLink to="/delivery-profiles/3">edge</RecordLink>
            </TableCell>
          </RecordRow>
        </TableBody>
      </Table>
    </MemoryRouter>
  )
  expect(markup).toContain('<tr class="')
  expect(markup).toContain('has-[a[data-record-link]]:cursor-pointer')
  expect(markup).toContain('border-b')
  expect(markup).not.toContain('tabindex')
  expect(markup).not.toContain(' role="')
})

// A minimal element tree whose `closest` matches tags and roles like the DOM.
interface FakeNode {
  tag: string
  role?: string
  parent?: FakeNode
}

function el(node: FakeNode): Element {
  return {
    node,
    contains(other: Element) {
      for (
        let n: FakeNode | undefined = (other as unknown as { node: FakeNode })
          .node;
        n;
        n = n.parent
      ) {
        if (n === node) return true
      }
      return false
    },
    closest(selector: string) {
      const wanted = selector.split(',')
      for (let n: FakeNode | undefined = node; n; n = n.parent) {
        if (
          wanted.includes(n.tag) ||
          (n.role && wanted.includes(`[role=${n.role}]`))
        ) {
          return el(n)
        }
      }
      return null
    },
  } as unknown as Element
}

describe('followsRecordLink', () => {
  const row: FakeNode = { tag: 'tr' }
  const cell: FakeNode = { tag: 'td', parent: row }

  test('a click on row whitespace or plain text follows the link', () => {
    expect(followsRecordLink(el(row), el(cell))).toBe(true)
    expect(followsRecordLink(el(row), el({ tag: 'span', parent: cell }))).toBe(
      true
    )
  })

  test('controls in the row keep their own click, the link included', () => {
    const button: FakeNode = { tag: 'button', parent: cell }
    expect(followsRecordLink(el(row), el({ tag: 'svg', parent: button }))).toBe(
      false
    )
    expect(followsRecordLink(el(row), el({ tag: 'a', parent: cell }))).toBe(
      false
    )
    expect(
      followsRecordLink(
        el(row),
        el({ tag: 'span', role: 'checkbox', parent: cell })
      )
    ).toBe(false)
  })

  test('custom widgets are controls too, whatever element carries the role', () => {
    for (const role of ['tab', 'combobox', 'radio', 'option', 'slider']) {
      expect(
        followsRecordLink(el(row), el({ tag: 'div', role, parent: cell }))
      ).toBe(false)
    }
  })

  test('clicks from a portal the row opened never navigate', () => {
    expect(
      followsRecordLink(el(row), el({ tag: 'div', role: 'menuitem' }))
    ).toBe(false)
    expect(followsRecordLink(el(row), el({ tag: 'div' }))).toBe(false)
  })
})

afterEach(cleanup)

function renderRecords(children: React.ReactNode) {
  return render(
    <MemoryRouter initialEntries={['/']}>
      <Routes>
        <Route path="/" element={children} />
        <Route path="/records/:id" element={<p>Opened record</p>} />
      </Routes>
    </MemoryRouter>
  )
}

describe('RecordRow', () => {
  const table = (cells: React.ReactNode) => (
    <Table>
      <TableBody>
        <RecordRow>{cells}</RecordRow>
      </TableBody>
    </Table>
  )

  test('a click on the row opens its record', () => {
    const view = renderRecords(
      table(
        <>
          <TableCell>
            <RecordLink to="/records/1">first</RecordLink>
          </TableCell>
          <TableCell>meta</TableCell>
        </>
      )
    )
    fireEvent.click(view.getByText('meta'))
    expect(view.getByText('Opened record')).not.toBeNull()
  })

  test('a button in the row keeps its own click', () => {
    let pressed = 0
    const view = renderRecords(
      table(
        <>
          <TableCell>
            <RecordLink to="/records/1">first</RecordLink>
          </TableCell>
          <TableCell>
            <button type="button" onClick={() => pressed++}>
              Edit
            </button>
          </TableCell>
        </>
      )
    )
    fireEvent.click(view.getByRole('button', { name: 'Edit' }))
    expect(pressed).toBe(1)
    expect(view.queryByText('Opened record')).toBeNull()
  })

  test('custom widgets and marked values keep their own click', () => {
    let selected = 0
    const view = renderRecords(
      table(
        <>
          <TableCell>
            <RecordLink to="/records/1">first</RecordLink>
          </TableCell>
          <TableCell>
            <div role="tab" onClick={() => selected++}>
              Logs
            </div>
          </TableCell>
          <TableCell>
            <span data-row-ignore="">
              <span>revealed-value</span>
            </span>
          </TableCell>
        </>
      )
    )
    fireEvent.click(view.getByRole('tab', { name: 'Logs' }))
    expect(selected).toBe(1)
    fireEvent.click(view.getByText('revealed-value'))
    expect(view.queryByText('Opened record')).toBeNull()
  })

  test('a row with two record links cannot tell which is meant and stays put', () => {
    const view = renderRecords(
      table(
        <>
          <TableCell>
            <RecordLink to="/records/1">first</RecordLink>
          </TableCell>
          <TableCell>
            <RecordLink to="/records/2">second</RecordLink>
          </TableCell>
          <TableCell>meta</TableCell>
        </>
      )
    )
    fireEvent.click(view.getByText('meta'))
    expect(view.queryByText('Opened record')).toBeNull()
  })
})

test('a record list item opens its record like a row', () => {
  const view = renderRecords(
    <ul role="list">
      <RecordListItem>
        <RecordLink to="/records/3">provider</RecordLink>
        <span>details</span>
      </RecordListItem>
    </ul>
  )
  fireEvent.click(view.getByText('details'))
  expect(view.getByText('Opened record')).not.toBeNull()
})
