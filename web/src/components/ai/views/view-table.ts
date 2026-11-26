// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Status } from '@temps-sdk/ds'
import type { ComponentProps } from 'react'

type Tone = ComponentProps<typeof Status>['tone']

/** One cell as the catalog parses it: text, a console link or a state. */
export type ViewCell =
  string | { text: string; href?: string | null; tone?: Tone | null }

const cellText = (cell: ViewCell | undefined): string =>
  cell === undefined ? '' : typeof cell === 'string' ? cell : cell.text

// A figure as an answer writes one: "1,829", "39.2%", "4155 ms", "+422 ms",
// "~375 ms", "181 (so far)". A date or a version is not one.
const FIGURE = /^[~+\-−]?\d[\d.,]*\s?[a-zA-Z%]{0,3}(\s\(.*\))?$/

/**
 * Whether a column holds figures: every cell that says something is one.
 * Such a column is set like the console's own number columns, right-aligned
 * in tabular digits, so magnitudes can be compared down the column.
 */
export function isFigureColumn(rows: ViewCell[][], column: number): boolean {
  const said = rows
    .map((row) => cellText(row[column]).trim())
    .filter((text) => text !== '' && text !== '—' && text !== '-')
  return said.length > 0 && said.every((text) => FIGURE.test(text))
}
