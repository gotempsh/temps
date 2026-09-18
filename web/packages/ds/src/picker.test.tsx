// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Field } from './field'
import { Picker } from './picker'

test('picker preserves Field input identity and validation wiring through cmdk', () => {
  const html = renderToStaticMarkup(<Field label="Service" error="Choose a service">{props => <Picker inputProps={props} items={[]} onValueChange={() => {}} />}</Field>)
  const labelId = html.match(/<label[^>]*id="([^"]+)"/)?.[1]
  expect(labelId).toBeDefined()
  expect(html).toContain(`aria-labelledby="${labelId}"`)
  expect(html).toContain('aria-invalid="true"')
  expect(html).toContain(`id="${labelId?.replace(/-label$/, '')}"`)
})
