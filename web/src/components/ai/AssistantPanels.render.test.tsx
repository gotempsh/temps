// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import { AssistantOnboarding, AssistantStart } from './AssistantPanels'
import { NO_PROVIDER_REASON, PROVIDER_SETUP_PATH } from './assistant-state'

function render(node: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>
  )
}

describe('AssistantOnboarding', () => {
  const markup = render(
    <AssistantOnboarding
      reason={NO_PROVIDER_REASON}
      setupPath={PROVIDER_SETUP_PATH}
      cloudPath="/settings/cloud"
    />
  )

  test('states precisely what is missing', () => {
    expect(markup).toContain('No AI provider is configured')
  })

  test('is setup and nothing else: no things to ask before a key exists', () => {
    const heading = markup.indexOf('Connect an AI provider</h2>')
    const reason = markup.indexOf('No AI provider is configured')
    const providers = markup.indexOf('aria-label="AI provider"')
    expect(heading).toBeGreaterThan(-1)
    expect(reason).toBeGreaterThan(heading)
    expect(providers).toBeGreaterThan(reason)
    expect(markup).not.toContain('Things to ask')
    expect(markup).not.toContain('<ul')
    expect(markup.match(/<h2/g)).toHaveLength(1)
  })

  test('offers every supported provider, not one vendor', () => {
    for (const name of [
      'OpenRouter',
      'OpenAI',
      'Anthropic',
      'Google Gemini',
      'xAI',
      'OpenAI API',
    ])
      expect(markup).toContain(`>${name}</span>`)
    // One is chosen to start with, so the key field is there at once.
    expect(markup.match(/aria-pressed="true"/g)).toHaveLength(1)
  })

  test('takes the key inline, as a masked field that is never prefilled', () => {
    expect(markup).toContain('OpenRouter API key')
    expect(markup).toContain('type="password"')
    expect(markup).toContain('autoComplete="off"')
    expect(markup).not.toContain('value="')
    // Tried or connected here: both are buttons, neither a link away.
    expect(markup).toContain('>Connect</button>')
    expect(markup).toContain('>Test</button>')
    expect(markup).not.toContain('Add endpoint')
  })

  test('deep-links to the setup page, not documentation', () => {
    expect(markup).toContain(`href="${PROVIDER_SETUP_PATH.replace('?', '?')}"`)
    expect(markup).toContain('Open AI provider settings')
  })

  test('offers Temps Cloud only as a secondary route, and only when given', () => {
    expect(markup).toContain('href="/settings/cloud"')
    const without = render(
      <AssistantOnboarding
        reason={NO_PROVIDER_REASON}
        setupPath={PROVIDER_SETUP_PATH}
      />
    )
    expect(without).not.toContain('Temps Cloud')
  })
})

describe('AssistantOnboarding sizing', () => {
  const props = {
    reason: NO_PROVIDER_REASON,
    setupPath: PROVIDER_SETUP_PATH,
  }

  test('is compact in the dock', () => {
    const markup = render(<AssistantOnboarding {...props} />)
    expect(markup).not.toContain('text-4xl')
    expect(markup).not.toContain('h-12')
  })

  test('is larger and centred on the full-screen page', () => {
    const markup = render(<AssistantOnboarding {...props} roomy />)
    expect(markup).toContain('text-4xl')
    expect(markup).toContain('justify-center')
    expect(markup).toContain('h-12')
  })
})

describe('AssistantStart', () => {
  test('shows skeleton placeholders, not a spinner, while checking the provider', () => {
    const markup = render(<AssistantStart onStarted={() => {}} />)
    expect(markup).toContain('role="status"')
    expect(markup).toContain('animate-pulse')
    expect(markup).not.toContain('animate-spin')
  })
})
