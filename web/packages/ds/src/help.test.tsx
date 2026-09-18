// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Disclosure, HelpPopover } from './help'

test('optional help is named, closed initially, and cannot submit its surrounding form', () => {
  const html = renderToStaticMarkup(<form><HelpPopover label="About retention">Retention details</HelpPopover></form>)
  expect(html).toContain('type="button"')
  expect(html).toContain('aria-label="About retention"')
  expect(html).toContain('aria-expanded="false"')
  expect(html).not.toContain('Retention details')
})

test('long-form help uses a native disclosure with a visible topic', () => {
  const html = renderToStaticMarkup(<Disclosure label="Retention details">Older logs are removed.</Disclosure>)
  expect(html).toContain('<summary')
  expect(html).toContain('Retention details')
  expect(html).not.toContain(' open=')
})
