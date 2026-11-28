// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { shouldSubmitOnEnter } from './useEnterSubmit'

// Minimal stand-in for an HTMLElement: the helper only reads `tagName` and
// calls `closest` for the popover roles.
function element(tagName: string, insideRoles: string[] = []): HTMLElement {
  return {
    tagName: tagName.toUpperCase(),
    closest: (selector: string) =>
      insideRoles.some((role) => selector === `[role="${role}"]`) ? {} : null,
  } as unknown as HTMLElement
}

describe('shouldSubmitOnEnter', () => {
  test('submits from text inputs', () => {
    expect(shouldSubmitOnEnter(element('input'))).toBe(true)
  })

  test('lets links navigate instead of submitting', () => {
    expect(shouldSubmitOnEnter(element('a'))).toBe(false)
  })

  test('leaves textareas and buttons alone', () => {
    expect(shouldSubmitOnEnter(element('textarea'))).toBe(false)
    expect(shouldSubmitOnEnter(element('button'))).toBe(false)
  })

  test('leaves select popovers alone', () => {
    expect(shouldSubmitOnEnter(element('div', ['option']))).toBe(false)
    expect(shouldSubmitOnEnter(element('div', ['listbox']))).toBe(false)
  })
})
