// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { isValidElement, type ReactNode } from 'react'

function extractText(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(extractText).join('')
  if (isValidElement<{ children?: ReactNode }>(node))
    return extractText(node.props.children)
  return ''
}

/** A fenced block as React Markdown hands it to a `pre` override. */
export interface MarkdownFence {
  /** The word after the opening fence, as written; undefined when there is none. */
  language: string | undefined
  /** The block's text, without the newline that closes it. */
  code: string
}

/** Reads the language and text of the fenced block inside a Markdown `pre`. */
export function markdownFence(children: ReactNode): MarkdownFence {
  const props = isValidElement<{ className?: string; children?: ReactNode }>(
    children
  )
    ? children.props
    : { children }
  return {
    language: /language-([\w-]+)/.exec(props.className ?? '')?.[1],
    code: extractText(props.children).replace(/\n$/, ''),
  }
}
