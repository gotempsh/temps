// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { SandboxTerminalCard } from './SandboxTerminalCard'

installBrowserWindow()
afterEach(cleanup)

test('offers an Open terminal button and does not connect on its own', () => {
  const sockets: string[] = []
  const original = globalThis.WebSocket
  globalThis.WebSocket = class {
    constructor(url: string) {
      sockets.push(url)
    }
  } as unknown as typeof WebSocket
  try {
    const view = render(
      <SandboxTerminalCard sandboxId="sbx_1" unavailableReason={null} />
    )
    const open = view.getByRole('button', {
      name: 'Open terminal',
    }) as HTMLButtonElement
    expect(open.disabled).toBe(false)
    expect(view.getByText(/reconnecting resumes the same session/)).toBeTruthy()
    expect(sockets).toEqual([])
  } finally {
    globalThis.WebSocket = original
  }
})

test('explains why it is unavailable instead of disappearing', () => {
  const view = render(
    <SandboxTerminalCard
      sandboxId="sbx_1"
      unavailableReason="This sandbox is stopped. Resume it to open a terminal."
    />
  )
  expect(view.getByText('Terminal unavailable')).toBeTruthy()
  expect(view.getByText(/Resume it to open a terminal/)).toBeTruthy()
  expect(
    (view.getByRole('button', { name: 'Open terminal' }) as HTMLButtonElement)
      .disabled
  ).toBe(true)
})
