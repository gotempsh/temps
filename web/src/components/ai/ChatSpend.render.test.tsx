// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { CompactSection, ContextMeter } from './ChatSpend'

test('the meter shows the tokens against the window', () => {
  const markup = renderToStaticMarkup(
    <ContextMeter
      context={{ model: 'vendor/large', tokens: 38_500, window: 1_000_000 }}
    />
  )
  expect(markup).toContain('38.5k of 1.0M tokens (4%)')
  expect(markup).toContain('role="progressbar"')
  expect(markup).not.toContain('costs more with every message')
})

test('a nearly full context says why a long chat costs more', () => {
  const markup = renderToStaticMarkup(
    <ContextMeter
      context={{ model: 'vendor/small', tokens: 180_000, window: 200_000 }}
    />
  )
  expect(markup).toContain('costs more with every message')
  expect(markup).toContain('bg-red-500')
})

test('a model without a listed window shows tokens and no bar', () => {
  const markup = renderToStaticMarkup(
    <ContextMeter context={{ model: 'vendor/x', tokens: 1_200 }} />
  )
  expect(markup).toContain('1.2k tokens')
  expect(markup).not.toContain('role="progressbar"')
})

test('compacting is offered on a chat that was never summarized', () => {
  const markup = renderToStaticMarkup(
    <CompactSection
      summarizedMessages={0}
      pending={false}
      onCompact={() => {}}
    />
  )
  expect(markup).toContain('Compact now')
  expect(markup).toContain('Long chats are summarized automatically')
  expect(markup).not.toContain('disabled=""')
})

test('a summarized chat says how much was folded, and a running compaction is disabled', () => {
  const markup = renderToStaticMarkup(
    <CompactSection
      summarizedMessages={14}
      pending={true}
      onCompact={() => {}}
    />
  )
  expect(markup).toContain('14 earlier messages are summarized')
  expect(markup).toContain('Compacting…')
  expect(markup).toContain('disabled=""')
})

test('the meter reads like the context window: title, percent used, bar', () => {
  const markup = renderToStaticMarkup(
    <ContextMeter
      context={{ model: 'vendor/large', tokens: 148_900, window: 1_000_000 }}
    />
  )
  expect(markup).toContain('Context window')
  expect(markup).toContain('15% used')
  expect(markup).toContain('148.9k of 1.0M tokens (15%)')
})
