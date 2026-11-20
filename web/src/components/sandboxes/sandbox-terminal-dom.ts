// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { computeTerminalSize } from './sandbox-terminal'

const FALLBACK_CELL = { width: 9, height: 17 }

/** Cell size xterm measured for the current font, with a sane fallback. */
export function measureTerminalCell(host: HTMLElement) {
  const probe = host.querySelector<HTMLElement>('.xterm-char-measure-element')
  const length = probe?.textContent?.length ?? 0
  if (!probe || length === 0) return FALLBACK_CELL
  const rect = probe.getBoundingClientRect()
  if (rect.width === 0 || rect.height === 0) return FALLBACK_CELL
  return { width: rect.width / length, height: rect.height }
}

/**
 * Whole cells that fit in `host`, after its padding and the viewport's
 * scrollbar. Counting the padding or the scrollbar over-reports columns, and
 * the shell then wraps lines that the user can see are still short.
 */
export function measureTerminalHost(host: HTMLElement) {
  const style = window.getComputedStyle(host)
  const pixels = (value: string) => Number.parseFloat(value) || 0
  const width =
    host.clientWidth - pixels(style.paddingLeft) - pixels(style.paddingRight)
  const height =
    host.clientHeight - pixels(style.paddingTop) - pixels(style.paddingBottom)
  const viewport = host.querySelector<HTMLElement>('.xterm-viewport')
  const scrollbar = viewport
    ? Math.max(0, viewport.offsetWidth - viewport.clientWidth)
    : 0
  const cell = measureTerminalCell(host)
  return computeTerminalSize(
    Math.max(0, width - scrollbar),
    Math.max(0, height),
    cell.width,
    cell.height
  )
}

interface Resizable {
  cols: number
  rows: number
  resize(cols: number, rows: number): void
}

/**
 * Resize `term` to fill `host`. Returns false, leaving the terminal alone,
 * when `host` has no layout yet (hidden or collapsed): a 0×0 host would
 * otherwise shrink the PTY to the minimum and send that to the shell.
 */
export function fitTerminalToHost(term: Resizable, host: HTMLElement): boolean {
  if (host.clientWidth === 0 || host.clientHeight === 0) return false
  const { cols, rows } = measureTerminalHost(host)
  if (cols !== term.cols || rows !== term.rows) term.resize(cols, rows)
  return true
}
