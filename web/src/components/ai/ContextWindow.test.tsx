// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  ContextWindow,
  contextPercentage,
  type ContextUsage,
} from './ContextWindow'

const usage: ContextUsage = {
  used_tokens: 224794,
  limit_tokens: 258400,
  source: 'provider_reported',
  updated_at: '2026-09-11T12:00:00Z',
  model: 'model-a',
}
test('context percentage comes from current usage and an explicit limit', () => {
  expect(contextPercentage(usage)).toBe(87)
  expect(contextPercentage({ ...usage, limit_tokens: undefined })).toBeNull()
  expect(contextPercentage({ ...usage, limit_tokens: 0 })).toBeNull()
  expect(contextPercentage({ ...usage, used_tokens: -1 })).toBeNull()
  expect(contextPercentage(null)).toBeNull()
})
test('context control remains discoverable when usage is unknown', () => {
  const html = renderToStaticMarkup(<ContextWindow />)
  expect(html).toContain('Context window')
  expect(html).not.toContain('0%')
})
test('shows reported percentage but hides snapshots belonging to a different model', () => {
  expect(
    renderToStaticMarkup(<ContextWindow usage={usage} model="model-a" />)
  ).toContain('87%')
  expect(
    renderToStaticMarkup(<ContextWindow usage={usage} model="model-b" />)
  ).not.toContain('87%')
})
