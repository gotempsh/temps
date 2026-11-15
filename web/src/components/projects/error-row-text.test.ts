// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { errorRowText } from './error-row-text'

describe('errorRowText', () => {
  test('says the type and the message once each', () => {
    expect(
      errorRowText({
        error_type: '*payments.ProviderTimeoutError',
        title:
          '*payments.ProviderTimeoutError: payment provider did not respond within 10s',
        message_template: 'payment provider did not respond within 10s',
      })
    ).toEqual({
      type: '*payments.ProviderTimeoutError',
      headline: 'payment provider did not respond within 10s',
      detail: null,
    })
  })

  test('keeps a message that adds to the title', () => {
    expect(
      errorRowText({
        error_type: 'TypeError',
        title: "TypeError: Cannot read properties of undefined (reading 'id')",
        message_template: 'while rendering <CartLine> for line {index}',
      })
    ).toEqual({
      type: 'TypeError',
      headline: "Cannot read properties of undefined (reading 'id')",
      detail: 'while rendering <CartLine> for line {index}',
    })
  })

  test('leaves a title that does not start with the type as it is', () => {
    expect(
      errorRowText({
        error_type: 'ChunkLoadError',
        title: 'Loading chunk 8133 failed',
        message_template: 'Loading chunk 8133 failed',
      })
    ).toEqual({
      type: 'ChunkLoadError',
      headline: 'Loading chunk 8133 failed',
      detail: null,
    })
  })

  test('never empties the headline when the title is only the type', () => {
    expect(
      errorRowText({ error_type: 'KeyError', title: 'KeyError: ' })
    ).toEqual({ type: 'KeyError', headline: 'KeyError:', detail: null })
  })

  test('falls back to "error" without a type', () => {
    expect(errorRowText({ error_type: null, title: 'boom' }).type).toBe('error')
  })
})
