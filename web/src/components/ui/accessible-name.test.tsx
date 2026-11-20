// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Plus, Trash2 } from 'lucide-react'
import { renderToStaticMarkup } from 'react-dom/server'

import { hasExplicitName, hasTextContent } from './accessible-name'
import { Button } from './button'
import { CopyButton } from './copy-button'
import { MetricSummary } from '../data-display/MetricSummary'

describe('hasExplicitName', () => {
  test('accepts aria-label, aria-labelledby and title', () => {
    expect(hasExplicitName({ 'aria-label': 'Delete' })).toBe(true)
    expect(hasExplicitName({ 'aria-labelledby': 'heading' })).toBe(true)
    expect(hasExplicitName({ title: 'Delete' })).toBe(true)
  })

  test('rejects missing or blank names', () => {
    expect(hasExplicitName({})).toBe(false)
    expect(hasExplicitName({ 'aria-label': '   ' })).toBe(false)
  })
})

describe('hasTextContent', () => {
  test('finds visible and screen-reader-only text', () => {
    expect(hasTextContent('Save')).toBe(true)
    expect(
      hasTextContent(
        <>
          <Trash2 />
          <span className="sr-only">Delete</span>
        </>
      )
    ).toBe(true)
  })

  test('an icon on its own is not text', () => {
    expect(hasTextContent(<Plus />)).toBe(false)
    expect(hasTextContent(null)).toBe(false)
    expect(hasTextContent(false)).toBe(false)
  })
})

describe('icon-only controls expose a name', () => {
  test('an icon Button renders the aria-label it was given', () => {
    const markup = renderToStaticMarkup(
      <Button size="icon" variant="ghost" aria-label="Delete deployment">
        <Trash2 />
      </Button>
    )
    expect(markup).toContain('aria-label="Delete deployment"')
  })

  test('CopyButton is named even without a label prop', () => {
    const markup = renderToStaticMarkup(<CopyButton value="abc" minimal />)
    expect(markup).toMatch(/<button[^>]*aria-label="[^"]+"/)
  })

  test('CopyButton uses a specific label when one is given', () => {
    const markup = renderToStaticMarkup(
      <CopyButton value="abc" minimal label="Copy deploy key" />
    )
    expect(markup).toContain('aria-label="Copy deploy key"')
  })
})

describe('MetricSummary', () => {
  test('labels the summary on a group, not on the definition list', () => {
    const markup = renderToStaticMarkup(
      <MetricSummary metrics={[{ label: 'Visitors', value: 12 }]} />
    )
    // aria-label is prohibited on a role-less <dl>.
    expect(markup).toMatch(/<div[^>]*role="group"[^>]*aria-label="/)
    expect(markup).not.toMatch(/<dl[^>]*aria-label/)
  })
})
