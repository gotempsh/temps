// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  commentFormSchema,
  MAX_COMMENT_CHARS,
  MAX_SAVED_SEARCH_NAME_CHARS,
  savedSearchFormSchema,
} from './forms'

describe('commentFormSchema', () => {
  test('trims the comment it sends', () => {
    expect(
      commentFormSchema.parse({ body: '  fixed in the next deploy \n' })
    ).toEqual({
      body: 'fixed in the next deploy',
    })
  })

  test('refuses an empty or blank comment', () => {
    for (const body of ['', '   \n\t']) {
      const result = commentFormSchema.safeParse({ body })
      expect(result.success).toBe(false)
      expect(result.error?.issues[0]?.message).toBe(
        'Write something before sending the comment.'
      )
    }
  })

  test('accepts the longest comment and refuses one character more', () => {
    expect(
      commentFormSchema.safeParse({ body: 'a'.repeat(MAX_COMMENT_CHARS) })
        .success
    ).toBe(true)
    const result = commentFormSchema.safeParse({
      body: 'a'.repeat(MAX_COMMENT_CHARS + 1),
    })
    expect(result.success).toBe(false)
    expect(result.error?.issues[0]?.message).toBe(
      'A comment can be at most 10,000 characters.'
    )
  })
})

describe('savedSearchFormSchema', () => {
  test('trims the name and keeps the visibility', () => {
    expect(
      savedSearchFormSchema.parse({ name: ' Open and mine ', shared: true })
    ).toEqual({ name: 'Open and mine', shared: true })
  })

  test('refuses a blank name', () => {
    const result = savedSearchFormSchema.safeParse({
      name: '  ',
      shared: false,
    })
    expect(result.success).toBe(false)
    expect(result.error?.issues[0]?.message).toBe('Give the search a name.')
  })

  test('refuses a name longer than the server accepts', () => {
    const result = savedSearchFormSchema.safeParse({
      name: 'n'.repeat(MAX_SAVED_SEARCH_NAME_CHARS + 1),
      shared: false,
    })
    expect(result.success).toBe(false)
    expect(result.error?.issues[0]?.message).toBe(
      'A name can be at most 100 characters.'
    )
  })
})
