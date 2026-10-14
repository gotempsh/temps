// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach } from 'bun:test'

/**
 * Makes `window` the registered DOM window for the duration of each test.
 *
 * `setup-dom.ts` registers happy-dom for the whole run, but a few older tests
 * replace `globalThis.window` with a hand-rolled stub (some never restore it),
 * and `bun test` shares one global across files. Interactive component tests
 * call this once at the top of the file so they never inherit a stub; the
 * previous value is put back afterwards.
 */
export function installBrowserWindow() {
  let previous: PropertyDescriptor | undefined
  beforeEach(() => {
    previous = Object.getOwnPropertyDescriptor(globalThis, 'window')
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      writable: true,
      value: globalThis,
    })
  })
  afterEach(() => {
    if (previous) Object.defineProperty(globalThis, 'window', previous)
    else Reflect.deleteProperty(globalThis, 'window')
  })
}
