// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'

/**
 * Helpers for the "does this control have an accessible name?" question
 * (WCAG 4.1.2, axe `button-name`). Kept free of side effects so the shared
 * components and the tests can both use them.
 */

/** Props that give an element an accessible name without visible text. */
export function hasExplicitName(props: {
  'aria-label'?: unknown
  'aria-labelledby'?: unknown
  title?: unknown
}): boolean {
  const label = props['aria-label']
  const labelledBy = props['aria-labelledby']
  const title = props.title
  return (
    (typeof label === 'string' && label.trim().length > 0) ||
    (typeof labelledBy === 'string' && labelledBy.trim().length > 0) ||
    (typeof title === 'string' && title.trim().length > 0)
  )
}

/**
 * Whether any text can be found in a React node tree. Custom components are
 * opaque (they may render text), so one counts as text -- the check is meant
 * to flag only the unambiguous icon-only shape: host elements and `svg`-like
 * leaves with no string anywhere.
 */
export function hasTextContent(node: React.ReactNode): boolean {
  let found = false
  React.Children.forEach(node, (child) => {
    if (found || child === null || child === undefined) return
    if (typeof child === 'boolean') return
    if (typeof child === 'string') {
      if (child.trim().length > 0) found = true
      return
    }
    if (typeof child === 'number') {
      found = true
      return
    }
    if (React.isValidElement<{ children?: React.ReactNode }>(child)) {
      // An element rendered by a component (function/class/forwardRef object)
      // that takes no children could still render text; treat as text only
      // when it was handed children that contain some.
      if (typeof child.type === 'string') {
        if (hasTextContent(child.props.children)) found = true
        return
      }
      if (child.props.children !== undefined) {
        if (hasTextContent(child.props.children)) found = true
      }
    }
  })
  return found
}

const warned = new Set<string>()

/**
 * Development-only warning for a `size="icon"` button that has neither an
 * accessible name nor text. Logged once per distinct call site description.
 */
export function warnUnnamedIconButton(
  props: {
    'aria-label'?: unknown
    'aria-labelledby'?: unknown
    title?: unknown
    children?: React.ReactNode
  },
  where: string
): void {
  if (process.env.NODE_ENV !== 'development') return
  if (hasExplicitName(props) || hasTextContent(props.children)) return
  if (warned.has(where)) return
  warned.add(where)
  console.warn(
    `[a11y] <${where}> is icon-only and has no accessible name. Add aria-label="..." (what the button does) or an sr-only text child.`
  )
}

/**
 * Accessible name for a control that shows a current value, such as a combobox
 * button: the field label followed by what it shows ("Project: All projects").
 * The visible text stays inside the name (WCAG 2.5.3), so voice control that
 * says the visible words still works. Without a label the shown value is the
 * name.
 */
export function fieldValueName(
  label: string | undefined,
  shown: string
): string {
  return label ? `${label}: ${shown}` : shown
}
