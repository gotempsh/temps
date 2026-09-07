// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { renderToStaticMarkup } from 'react-dom/server'
import { AiHarnessLogo } from './ai-harness-logo'

describe('AiHarnessLogo', () => {
  test.each([
    ['claude_cli', 'Claude Code logo', '/ai-harnesses/claude-code.svg'],
    ['codex_cli', 'Codex logo', '/ai-harnesses/codex.svg'],
    ['opencode', 'OpenCode logo', '/ai-harnesses/opencode.svg'],
  ])('renders the official %s SVG asset', (providerId, label, src) => {
    const markup = renderToStaticMarkup(
      <AiHarnessLogo providerId={providerId} />
    )

    expect(markup).toContain(`data-harness="${providerId}"`)
    expect(markup).toContain(`aria-label="${label}"`)
    expect(markup).toContain(`src="${src}"`)
    expect(markup).toContain('<img')
  })

  test('normalizes provider aliases and keeps an unknown-provider fallback', () => {
    expect(
      renderToStaticMarkup(<AiHarnessLogo providerId="anthropic" />)
    ).toContain('data-harness="claude_cli"')
    expect(
      renderToStaticMarkup(<AiHarnessLogo providerId="custom_harness" />)
    ).toContain('data-harness="custom_harness"')
  })

  test('keeps the Codex SVG background transparent', () => {
    const svg = readFileSync(
      new URL('../../../public/ai-harnesses/codex.svg', import.meta.url),
      'utf8'
    )

    expect(svg).toContain('<path')
    expect(svg).not.toContain('<rect')
    expect(svg).not.toContain('background')
  })
})
