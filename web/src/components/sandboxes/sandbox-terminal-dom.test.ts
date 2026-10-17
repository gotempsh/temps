// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import {
  fitTerminalToHost,
  measureTerminalCell,
  measureTerminalHost,
} from './sandbox-terminal-dom'

interface FakeHostOptions {
  clientWidth: number
  clientHeight: number
  padding?: { left: number; right: number; top: number; bottom: number }
  /** One character of xterm's measuring element: its text and box. */
  cell?: { text: string; width: number; height: number } | null
  /** The viewport's offset/client width; their difference is the scrollbar. */
  viewport?: { offsetWidth: number; clientWidth: number } | null
}

// The helpers only read a handful of properties, so a duck-typed host keeps
// these tests independent of a global DOM.
function fakeHost(options: FakeHostOptions): HTMLElement {
  const probe = options.cell
    ? {
        textContent: options.cell.text,
        getBoundingClientRect: () => ({
          width: options.cell?.width ?? 0,
          height: options.cell?.height ?? 0,
        }),
      }
    : null
  const viewport = options.viewport ?? null
  return {
    clientWidth: options.clientWidth,
    clientHeight: options.clientHeight,
    padding: options.padding,
    querySelector: (selector: string) => {
      if (selector === '.xterm-char-measure-element') return probe
      if (selector === '.xterm-viewport') return viewport
      return null
    },
  } as unknown as HTMLElement
}

const originalWindow = (globalThis as { window?: unknown }).window

function installComputedStyle() {
  ;(globalThis as { window?: unknown }).window = {
    getComputedStyle: (host: { padding?: Record<string, number> }) => ({
      paddingLeft: `${host.padding?.left ?? 0}px`,
      paddingRight: `${host.padding?.right ?? 0}px`,
      paddingTop: `${host.padding?.top ?? 0}px`,
      paddingBottom: `${host.padding?.bottom ?? 0}px`,
    }),
  }
}

afterEach(() => {
  ;(globalThis as { window?: unknown }).window = originalWindow
})

describe('measureTerminalCell', () => {
  test('divides the measuring element by its character count', () => {
    const host = fakeHost({
      clientWidth: 800,
      clientHeight: 400,
      cell: { text: 'WWWWWWWWWW', width: 100, height: 20 },
    })

    expect(measureTerminalCell(host)).toEqual({ width: 10, height: 20 })
  })

  test('falls back when xterm has not rendered a measuring element', () => {
    const host = fakeHost({ clientWidth: 800, clientHeight: 400 })

    expect(measureTerminalCell(host)).toEqual({ width: 9, height: 17 })
  })

  test('falls back for an empty or zero-sized measuring element', () => {
    const empty = fakeHost({
      clientWidth: 800,
      clientHeight: 400,
      cell: { text: '', width: 100, height: 20 },
    })
    const collapsed = fakeHost({
      clientWidth: 800,
      clientHeight: 400,
      cell: { text: 'W', width: 0, height: 0 },
    })

    expect(measureTerminalCell(empty)).toEqual({ width: 9, height: 17 })
    expect(measureTerminalCell(collapsed)).toEqual({ width: 9, height: 17 })
  })
})

describe('measureTerminalHost', () => {
  test('counts whole cells inside the padding', () => {
    installComputedStyle()
    const host = fakeHost({
      clientWidth: 820,
      clientHeight: 440,
      padding: { left: 10, right: 10, top: 10, bottom: 10 },
      cell: { text: 'W', width: 10, height: 20 },
    })

    // 800x420 usable at 10x20 -> 80 columns, 21 rows.
    expect(measureTerminalHost(host)).toEqual({ cols: 80, rows: 21 })
  })

  test('does not count the scrollbar as columns', () => {
    installComputedStyle()
    const host = fakeHost({
      clientWidth: 800,
      clientHeight: 400,
      cell: { text: 'W', width: 10, height: 20 },
      viewport: { offsetWidth: 800, clientWidth: 785 },
    })

    // 15px scrollbar leaves 785px: 78 whole columns, not 80.
    expect(measureTerminalHost(host)).toEqual({ cols: 78, rows: 20 })
  })

  test('never reports a negative usable area for an over-padded host', () => {
    installComputedStyle()
    const host = fakeHost({
      clientWidth: 10,
      clientHeight: 10,
      padding: { left: 40, right: 40, top: 40, bottom: 40 },
      cell: { text: 'W', width: 10, height: 20 },
    })

    const { cols, rows } = measureTerminalHost(host)

    expect(cols).toBeGreaterThan(0)
    expect(rows).toBeGreaterThan(0)
  })
})

describe('fitTerminalToHost', () => {
  function fakeTerminal(cols: number, rows: number) {
    const calls: Array<[number, number]> = []
    return {
      cols,
      rows,
      calls,
      resize(nextCols: number, nextRows: number) {
        calls.push([nextCols, nextRows])
        this.cols = nextCols
        this.rows = nextRows
      },
    }
  }

  test('leaves the terminal alone while the host has no layout', () => {
    installComputedStyle()
    const terminal = fakeTerminal(120, 40)

    for (const [width, height] of [
      [0, 400],
      [800, 0],
      [0, 0],
    ]) {
      const fitted = fitTerminalToHost(
        terminal,
        fakeHost({ clientWidth: width, clientHeight: height })
      )
      expect(fitted).toBe(false)
    }

    expect(terminal.calls).toEqual([])
  })

  test('resizes to the measured cells when the size changed', () => {
    installComputedStyle()
    const terminal = fakeTerminal(80, 24)
    const host = fakeHost({
      clientWidth: 1000,
      clientHeight: 400,
      cell: { text: 'W', width: 10, height: 20 },
    })

    expect(fitTerminalToHost(terminal, host)).toBe(true)
    expect(terminal.calls).toEqual([[100, 20]])
  })

  test('does not resize, and so does not signal the shell, when unchanged', () => {
    installComputedStyle()
    const terminal = fakeTerminal(100, 20)
    const host = fakeHost({
      clientWidth: 1000,
      clientHeight: 400,
      cell: { text: 'W', width: 10, height: 20 },
    })

    expect(fitTerminalToHost(terminal, host)).toBe(true)
    expect(terminal.calls).toEqual([])
  })
})
