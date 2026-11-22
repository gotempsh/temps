// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import { AskBeforeChangesToggle } from './AskBeforeChangesToggle'
import { AssistantOnboarding, AssistantStart } from './AssistantPanels'
import {
  ASSISTANT_EXAMPLE_PROMPTS,
  NO_PROVIDER_REASON,
  OPENROUTER_SETUP_PATH,
} from './assistant-state'

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
      setupPath={OPENROUTER_SETUP_PATH}
      cloudPath="/settings/cloud"
    />
  )

  test('states precisely what is missing', () => {
    expect(markup).toContain('No AI provider is configured')
  })

  test('shows a concrete example of what it would do', () => {
    expect(markup).toContain(ASSISTANT_EXAMPLE_PROMPTS[0])
    expect(markup).toContain('Deletions always wait for your approval')
  })

  test('takes the key inline, as a masked field that is never prefilled', () => {
    expect(markup).toContain('OpenRouter API key')
    expect(markup).toContain('type="password"')
    expect(markup).toContain('autoComplete="off"')
    expect(markup).not.toContain('value="')
    expect(markup).toContain('Save key')
  })

  test('deep-links to the setup page, not documentation', () => {
    expect(markup).toContain(
      `href="${OPENROUTER_SETUP_PATH.replace('?', '?')}"`
    )
    expect(markup).toContain('Open AI provider settings')
  })

  test('offers Temps Cloud only as a secondary route, and only when given', () => {
    expect(markup).toContain('href="/settings/cloud"')
    const without = render(
      <AssistantOnboarding
        reason={NO_PROVIDER_REASON}
        setupPath={OPENROUTER_SETUP_PATH}
      />
    )
    expect(without).not.toContain('Temps Cloud')
  })
})

describe('AssistantOnboarding sizing', () => {
  const props = {
    reason: NO_PROVIDER_REASON,
    setupPath: OPENROUTER_SETUP_PATH,
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

describe('AskBeforeChangesToggle', () => {
  test('is off when changes run automatically', () => {
    const markup = render(
      <AskBeforeChangesToggle permissionModeId="auto" onChange={() => {}} />
    )
    expect(markup).toContain('Ask before changes')
    expect(markup).toContain('aria-checked="false"')
  })

  test('is on when every change asks first', () => {
    const markup = render(
      <AskBeforeChangesToggle
        permissionModeId="confirm-actions"
        onChange={() => {}}
      />
    )
    expect(markup).toContain('aria-checked="true"')
  })

  test('says deletions always ask, so the off state is not read as "no safety"', () => {
    const markup = render(
      <AskBeforeChangesToggle permissionModeId="auto" onChange={() => {}} />
    )
    expect(markup).toContain('Deletions always ask')
  })

  test('can be disabled while a turn is running', () => {
    const markup = render(
      <AskBeforeChangesToggle
        permissionModeId="auto"
        disabled
        onChange={() => {}}
      />
    )
    expect(markup).toContain('disabled=""')
  })
})
