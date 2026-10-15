// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { ScrollArea } from './scroll-area'
import { ScrollRegion } from './scroll-region'
import { isOverflowing, scrollableRegionProps } from './scrollable-region'
import { Table, TableBody, TableCell, TableRow } from './table'

const box = (
  scrollWidth: number,
  clientWidth: number,
  scrollHeight = 100,
  clientHeight = 100
) => ({ scrollWidth, clientWidth, scrollHeight, clientHeight })

describe('isOverflowing', () => {
  test('is true when content is wider or taller than the box', () => {
    expect(isOverflowing(box(400, 200))).toBe(true)
    expect(isOverflowing(box(200, 200, 500, 100))).toBe(true)
  })

  test('is false when content fits, ignoring one pixel of rounding', () => {
    expect(isOverflowing(box(200, 200))).toBe(false)
    expect(isOverflowing(box(201, 200))).toBe(false)
    expect(isOverflowing(box(200, 200, 101, 100))).toBe(false)
  })
})

describe('scrollableRegionProps', () => {
  test('adds nothing while the content fits, so there is no extra tab stop', () => {
    expect(scrollableRegionProps(false, 'Deployments')).toEqual({})
    expect(scrollableRegionProps(false)).toEqual({})
  })

  test('makes an overflowing container focusable', () => {
    expect(scrollableRegionProps(true)).toEqual({ tabIndex: 0 })
  })

  test('names the container as a region when a label is supplied', () => {
    // aria-label is only valid on an element with a role.
    expect(scrollableRegionProps(true, 'Deployments, scrollable')).toEqual({
      tabIndex: 0,
      role: 'region',
      'aria-label': 'Deployments, scrollable',
    })
  })
})

describe('scroll containers render without prohibited attributes', () => {
  test('the shared Table keeps its overflow wrapper and the table name', () => {
    const markup = renderToStaticMarkup(
      <Table aria-label="Deployments">
        <TableBody>
          <TableRow>
            <TableCell>build</TableCell>
          </TableRow>
        </TableBody>
      </Table>
    )
    expect(markup).toContain('overflow-auto')
    // The name lives on the <table>; the wrapper only gets a name once it
    // actually scrolls (and then together with role="region").
    expect(markup).toMatch(/<table[^>]*aria-label="Deployments"/)
    expect(markup).not.toMatch(/<div[^>]*aria-label/)
  })

  test('ScrollArea never puts aria-label on its role-less root', () => {
    const markup = renderToStaticMarkup(
      <ScrollArea aria-label="Build output" className="h-24">
        <p>line</p>
      </ScrollArea>
    )
    expect(markup).toContain('data-radix-scroll-area-viewport')
    // The root div is the first element; the label is held back for the
    // viewport so it only appears together with role="region".
    const root = /^<div[^>]*>/.exec(markup)?.[0] ?? ''
    expect(root).not.toContain('aria-label')
  })

  test('ScrollRegion is a plain overflow container until it scrolls', () => {
    const markup = renderToStaticMarkup(
      <ScrollRegion className="rounded-lg border" regionLabel="Results">
        <table>
          <tbody>
            <tr>
              <td>cell</td>
            </tr>
          </tbody>
        </table>
      </ScrollRegion>
    )
    expect(markup).toContain('overflow-auto')
    expect(markup).not.toContain('tabindex')
    expect(markup).not.toContain('role=')
  })
})
