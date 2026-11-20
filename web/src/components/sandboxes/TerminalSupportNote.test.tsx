// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  TerminalSupportNote,
  type TerminalCapability,
} from './TerminalSupportNote'

const unstaged: TerminalCapability = {
  supported: false,
  reason: 'the guest terminal agent (temps-pty-agent) is not staged',
  setup_command: 'temps firecracker setup --pty-agent-bin <path>',
}

function render(capability: TerminalCapability | undefined) {
  return renderToStaticMarkup(<TerminalSupportNote capability={capability} />)
}

describe('TerminalSupportNote', () => {
  test('states what is missing and shows the fix', () => {
    const html = render(unstaged)
    expect(html).toContain('Terminals unavailable')
    expect(html).toContain('temps-pty-agent')
    expect(html).toContain('temps firecracker setup')
  })

  test('says why even when there is no command to run', () => {
    const html = render({
      supported: false,
      reason: 'the local backend has no terminal agent',
      setup_command: null,
    })
    expect(html).toContain('the local backend has no terminal agent')
    expect(html).not.toContain('<code')
  })

  test('renders nothing while loading or when terminals work', () => {
    expect(render(undefined)).toBe('')
    expect(render({ supported: true, reason: null, setup_command: null })).toBe(
      ''
    )
  })
})
