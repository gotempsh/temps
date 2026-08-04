import { afterEach, describe, expect, mock, test } from 'bun:test'

import { writeToClipboard } from '@/lib/clipboard'

const originalNavigator = globalThis.navigator
const originalDocument = globalThis.document

afterEach(() => {
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: originalNavigator,
  })
  Object.defineProperty(globalThis, 'document', {
    configurable: true,
    value: originalDocument,
  })
})

describe('writeToClipboard', () => {
  test('uses the secure-context Clipboard API when available', async () => {
    const writeText = mock(async (_value: string) => undefined)
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { clipboard: { writeText } },
    })

    await writeToClipboard('alert proposal')
    expect(writeText).toHaveBeenCalledWith('alert proposal')
  })

  test('falls back to selection copy and restores focus', async () => {
    const focus = mock(() => undefined)
    const textarea = {
      value: '',
      style: {},
      setAttribute: mock(() => undefined),
      select: mock(() => undefined),
      setSelectionRange: mock(() => undefined),
    }
    const body = {
      appendChild: mock(() => undefined),
      removeChild: mock(() => undefined),
    }
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: {},
    })
    Object.defineProperty(globalThis, 'document', {
      configurable: true,
      value: {
        activeElement: { focus },
        body,
        createElement: () => textarea,
        execCommand: () => true,
      },
    })

    await writeToClipboard('fallback')
    expect(textarea.value).toBe('fallback')
    expect(textarea.setSelectionRange).toHaveBeenCalledWith(0, 8)
    expect(body.removeChild).toHaveBeenCalledWith(textarea)
    expect(focus).toHaveBeenCalled()
  })

  test('rejects when the fallback copy is refused and still cleans up', async () => {
    const textarea = {
      value: '',
      style: {},
      setAttribute: () => undefined,
      select: () => undefined,
      setSelectionRange: () => undefined,
    }
    const removeChild = mock(() => undefined)
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: {},
    })
    Object.defineProperty(globalThis, 'document', {
      configurable: true,
      value: {
        activeElement: null,
        body: { appendChild: () => undefined, removeChild },
        createElement: () => textarea,
        execCommand: () => false,
      },
    })

    await expect(writeToClipboard('blocked')).rejects.toThrow(
      'browser refused the copy'
    )
    expect(removeChild).toHaveBeenCalledWith(textarea)
  })
})
