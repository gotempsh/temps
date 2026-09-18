// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Field, FormErrors } from './field'

test('field links its label, help and error to the control', () => {
  const html = renderToStaticMarkup(<Field label="Project" description="Choose a name" error="Required">{props => <input {...props} />}</Field>)
  expect(html).toContain('aria-invalid="true"')
  const id = html.match(/<input id="([^"]+)"/)?.[1]
  expect(id).toBeDefined()
  expect(html).toContain(`for="${id}"`)
  expect(html).toContain(`aria-describedby="${id}-description ${id}-error"`)
})

test('summary identifies which fields have the same validation message', () => {
  const html = renderToStaticMarkup(<FormErrors errors={{Project: 'Required', Service: 'Required', Email: undefined}} />)
  expect(html).toContain('Project:')
  expect(html).toContain('Service:')
  expect(html).not.toContain('Email:')
  expect(html).toContain('Fix 2 fields')
})
