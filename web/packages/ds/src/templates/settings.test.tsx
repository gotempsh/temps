// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { Children, isValidElement, type FormEvent } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { Settings } from './settings'

for (const state of [
  { dirty: false, saving: false, errors: {} },
  { dirty: true, saving: true, errors: {} },
  { dirty: true, saving: false, errors: { Name: 'Required' } },
  { dirty: true, saving: false, errors: {} },
]) {
  test(`guards form submission: ${JSON.stringify(state)}`, () => {
    let submitted = false
    let prevented = false
    const tree = Settings({ ...state, title: 'Settings', children: null, onSubmit: () => { submitted = true } })
    const form = Children.toArray(tree.props.children).find(child => isValidElement(child) && child.type === 'form')
    if (!isValidElement<{onSubmit: (event: FormEvent<HTMLFormElement>) => void}>(form)) throw new Error('Missing settings form')
    form.props.onSubmit({ preventDefault: () => { prevented = true } } as FormEvent<HTMLFormElement>)
    const allowed = state.dirty && !state.saving && !Object.values(state.errors).some(Boolean)
    expect(submitted).toBe(allowed)
    expect(prevented).toBe(!allowed)
  })
}

test('invalid dirty forms show an inert save button', () => {
  const html = renderToStaticMarkup(<Settings title="Settings" dirty errors={{Name: 'Required'}} onSubmit={() => {}}>{null}</Settings>)
  expect(html).toContain('aria-disabled="true"')
  expect(html).toContain('pointer-events-none opacity-50')
})
