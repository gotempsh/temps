// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'

import { followTranscriptGrowth } from './transcript-follow'

type Callback = (records: unknown[]) => void

let resizeCallbacks: Callback[] = []
let mutationCallbacks: Callback[] = []
let observed: Set<unknown>
const original = {
  resize: globalThis.ResizeObserver,
  mutation: globalThis.MutationObserver,
}

beforeEach(() => {
  resizeCallbacks = []
  mutationCallbacks = []
  observed = new Set()
  globalThis.ResizeObserver = class {
    constructor(callback: Callback) {
      resizeCallbacks.push(callback)
    }
    observe(node: unknown) {
      observed.add(node)
    }
    unobserve(node: unknown) {
      observed.delete(node)
    }
    disconnect() {
      observed.clear()
    }
  } as unknown as typeof ResizeObserver
  globalThis.MutationObserver = class {
    constructor(callback: Callback) {
      mutationCallbacks.push(callback)
    }
    observe() {}
    disconnect() {}
  } as unknown as typeof MutationObserver
})

afterEach(() => {
  globalThis.ResizeObserver = original.resize
  globalThis.MutationObserver = original.mutation
})

function viewport(rows: unknown[]) {
  return {
    scrollHeight: 500,
    scrollTop: 0,
    children: rows as unknown as HTMLCollection,
  }
}

const resized = () => resizeCallbacks.forEach((callback) => callback([]))

test('a followed transcript scrolls to its end when a row grows', () => {
  const view = viewport([{}, {}])
  followTranscriptGrowth(view, () => true)
  expect(observed.size).toBe(2)

  view.scrollHeight = 740 // suggestions loaded under the last answer
  resized()
  expect(view.scrollTop).toBe(740)
})

test('a reader who scrolled up is left where they are', () => {
  const view = viewport([{}])
  followTranscriptGrowth(view, () => false)
  view.scrollHeight = 740
  resized()
  expect(view.scrollTop).toBe(0)
})

test('content that shrinks never moves the view', () => {
  const view = viewport([{}])
  view.scrollTop = 120
  followTranscriptGrowth(view, () => true)
  view.scrollHeight = 300
  resized()
  expect(view.scrollTop).toBe(120)
})

test('rows added later are watched and removed rows are let go', () => {
  const view = viewport([])
  followTranscriptGrowth(view, () => true)
  const card = { nodeType: 1 }
  const text = { nodeType: 3 }
  view.scrollHeight = 620
  mutationCallbacks[0]([{ addedNodes: [card, text], removedNodes: [] }])
  expect(observed.has(card)).toBe(true)
  expect(observed.has(text)).toBe(false)
  expect(view.scrollTop).toBe(620)

  mutationCallbacks[0]([{ addedNodes: [], removedNodes: [card] }])
  expect(observed.has(card)).toBe(false)
})

test('stopping disconnects the observers', () => {
  const view = viewport([{}])
  const stop = followTranscriptGrowth(view, () => true)
  stop()
  expect(observed.size).toBe(0)
})
