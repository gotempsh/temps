// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { ModelVendorSelect } from './AssistantModelPicker'
import { ModelVendorLogo } from './ModelVendorLogo'

const vendors = [
  { key: 'google', name: 'Google', logo: 'google.svg', count: 44 },
  { key: 'qwen', name: 'Qwen', logo: null, count: 43 },
]

test('the filter is one control that says what it filters', () => {
  const markup = renderToStaticMarkup(
    <ModelVendorSelect
      vendors={vendors}
      total={87}
      value={null}
      onChange={() => {}}
    />
  )
  expect(markup).toContain('aria-label="Filter models by vendor"')
  expect(markup).toContain('role="combobox"')
})

test('a single vendor needs no filter', () => {
  const markup = renderToStaticMarkup(
    <ModelVendorSelect
      vendors={vendors.slice(0, 1)}
      total={44}
      value={null}
      onChange={() => {}}
    />
  )
  expect(markup).toBe('')
})

test('a vendor without a mark gets its initial, an unnamed one a generic icon', () => {
  const lettered = renderToStaticMarkup(
    <ModelVendorLogo vendor={{ name: 'Qwen', logo: null }} />
  )
  expect(lettered).toContain('>Q<')
  expect(lettered).not.toContain('<img')
  const generic = renderToStaticMarkup(
    <ModelVendorLogo vendor={{ name: 'Other', logo: null }} />
  )
  expect(generic).toContain('<svg')
  const marked = renderToStaticMarkup(
    <ModelVendorLogo vendor={{ name: 'Google', logo: 'google.svg' }} />
  )
  expect(marked).toContain('<img')
})
