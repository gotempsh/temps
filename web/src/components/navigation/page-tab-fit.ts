// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Which tabs a strip shows when it is `available` wide, by index and in
 * order; the rest are listed under "More". A strip never scrolls sideways
 * and never takes a second line.
 *
 * The tab shown is always among them: it takes the last place when it falls
 * past what fits, so the strip never loses where the user is. A strip that
 * was not measured (`available` of 0) shows every tab.
 */
export function fittingTabs(
  widths: readonly number[],
  {
    available,
    gap,
    more,
    trailing = 0,
    current,
  }: {
    available: number
    /** The space between two tabs. */
    gap: number
    /** The width of the "More" control, which shows only when needed. */
    more: number
    /** A control after the last tab, when the strip has one. */
    trailing?: number
    /** The index of the tab shown. */
    current?: number
  }
): number[] {
  const all = widths.map((_, index) => index)
  const span = (indexes: readonly number[], extras: readonly number[]) => {
    const parts = [
      ...indexes.map((index) => widths[index]),
      ...extras.filter((width) => width > 0),
    ]
    return (
      parts.reduce((sum, width) => sum + width, 0) +
      gap * Math.max(0, parts.length - 1)
    )
  }
  if (available <= 0 || span(all, [trailing]) <= available) return all
  const marked =
    current !== undefined && current >= 0 && current < widths.length
      ? current
      : undefined
  for (let count = widths.length - 1; count >= 1; count--) {
    const shown = all.slice(0, count)
    if (marked !== undefined && marked >= count) shown[count - 1] = marked
    if (span(shown, [more, trailing]) <= available) return shown
  }
  // Nothing fits beside "More": the tab shown stays, and is cut short.
  return widths.length === 0 ? [] : [marked ?? 0]
}
