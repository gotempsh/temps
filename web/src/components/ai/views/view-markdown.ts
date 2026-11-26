// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Spec, UIElement } from '@json-render/core'
import { VIEW_FENCE } from './prompt'
import { parseViewSpec } from './view-spec'

type Props = Record<string, unknown>
interface Linked {
  text: string
  href?: string | null
}

/** Text safe inside a Markdown table cell. */
const cellText = (text: string) =>
  text.replace(/\|/g, '\\|').replace(/\s*\n\s*/g, ' ')

const linked = ({ text, href }: Linked) =>
  typeof href === 'string' && href.startsWith('/') && !href.startsWith('//')
    ? `[${text}](${href})`
    : text

function table(columns: string[], rows: string[][]): string {
  const line = (cells: string[]) => `| ${cells.map(cellText).join(' | ')} |`
  return [
    line(columns),
    line(columns.map(() => '---')),
    ...rows.map((row) => line(columns.map((_, index) => row[index] ?? ''))),
  ].join('\n')
}

/** One element as the Markdown that says the same thing. */
function elementMarkdown(spec: Spec, key: string): string {
  const element: UIElement | undefined = spec.elements[key]
  if (!element) return ''
  const props = element.props as Props
  const children = (separator: string) =>
    (element.children ?? [])
      .map((child) => elementMarkdown(spec, child))
      .filter((text) => text !== '')
      .join(separator)

  switch (element.type) {
    case 'Stack':
      return children('\n\n')
    case 'Stats':
      return children('\n')
    case 'Stat': {
      const { label, value, detail } = props as {
        label: string
        value: string
        detail?: string | null
      }
      return `- ${label}: **${value}**${detail ? ` (${detail})` : ''}`
    }
    case 'Status':
      return String(props.label)
    case 'KeyValues': {
      const { items } = props as {
        items: Array<{ label: string; value: string; href?: string | null }>
      }
      return items
        .map(
          (item) =>
            `- ${item.label}: ${linked({ text: item.value, href: item.href })}`
        )
        .join('\n')
    }
    case 'Table': {
      const { columns, rows } = props as {
        columns: string[]
        rows: Array<Array<string | Linked>>
      }
      return table(
        columns,
        rows.map((row) =>
          row.map((cell) => (typeof cell === 'string' ? cell : linked(cell)))
        )
      )
    }
    case 'Trend': {
      const { label, points, partial } = props as {
        label: string
        points: Array<{ at: string; value: number }>
        partial?: boolean | null
      }
      const last = points.length - 1
      return table(
        ['', label],
        points.map((point, index) => [
          point.at,
          `${point.value}${partial && index === last ? ' (so far)' : ''}`,
        ])
      )
    }
    case 'Bars': {
      const { items } = props as {
        items: Array<Linked & { label: string; value: number }>
      }
      return items
        .map(
          (item) =>
            `- ${linked({ text: item.label, href: item.href })}: ${item.text ?? item.value}`
        )
        .join('\n')
    }
    case 'Callout':
      return `> **${String(props.title)}** ${String(props.text)}`
    case 'Link':
      return linked({ text: String(props.label), href: String(props.href) })
    default:
      return ''
  }
}

const VIEW_BLOCK = new RegExp(
  '```' + VIEW_FENCE + '[^\\n]*\\n([\\s\\S]*?)\\n```',
  'g'
)

/**
 * An answer's text with each view written out as Markdown: what "Copy
 * message" puts on the clipboard, so a pasted answer carries the figures the
 * view showed rather than the patch lines it was drawn from. A block that
 * describes no view is left as it was written.
 */
export function viewsAsMarkdown(text: string): string {
  return text.replace(VIEW_BLOCK, (block: string, source: string) => {
    const view = parseViewSpec(source)
    if (!view) return block
    return elementMarkdown(view.spec, view.spec.root) || block
  })
}
