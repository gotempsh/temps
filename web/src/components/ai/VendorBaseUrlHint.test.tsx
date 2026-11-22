// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { VendorBaseUrlHint } from './VendorBaseUrlHint'

function render(open: boolean) {
  return renderToStaticMarkup(
    <VendorBaseUrlHint open={open} providerName="OpenAI" onSwitch={() => {}} />
  )
}

test('renders nothing without a custom base URL on a vendor provider', () => {
  expect(render(false)).toBe('')
})

test('explains the vendor model check and offers the OpenAI-compatible switch', () => {
  const html = render(true)
  expect(html).toContain('verified with OpenAI&#x27;s own models')
  expect(html).toContain('add an OpenAI-compatible provider')
  expect(html).toContain('Switch to OpenAI-compatible')
})
