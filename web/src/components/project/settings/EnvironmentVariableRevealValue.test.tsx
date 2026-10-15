// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  EnvironmentVariableRevealValue,
  EnvironmentVariableRevealView,
} from './EnvironmentVariableRevealValue'

function render(isSecret: boolean) {
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <EnvironmentVariableRevealValue
        projectId={1}
        variableId={7}
        name="APP_URL"
        isSecret={isSecret}
        version="1"
      />
    </QueryClientProvider>
  )
}

describe('environment variable detail value', () => {
  test('regular variables offer an audited reveal instead of a dead mask', () => {
    const html = render(false)
    expect(html).toContain('aria-label="Reveal APP_URL"')
    expect(html).toContain('Reveal to view')
    expect(html).not.toContain('Hidden in this view')
  })

  test('secret variables stay masked with no reveal control', () => {
    const html = render(true)
    expect(html).toContain('Secret · write-only')
    expect(html).not.toContain('Reveal APP_URL')
  })

  test('a revealed value is shown with a hide control', () => {
    const html = renderToStaticMarkup(
      <EnvironmentVariableRevealView
        name="APP_URL"
        state={{ phase: 'revealed', value: 'https://app.example.test' }}
        onReveal={() => undefined}
        onHide={() => undefined}
      />
    )
    expect(html).toContain('https://app.example.test')
    expect(html).toContain('aria-label="Hide APP_URL"')
    expect(html).not.toContain('••••')
  })

  test('an in-flight reveal is masked and cannot be clicked again', () => {
    const html = renderToStaticMarkup(
      <EnvironmentVariableRevealView
        name="APP_URL"
        state={{ phase: 'revealing' }}
        onReveal={() => undefined}
        onHide={() => undefined}
      />
    )
    expect(html).toContain('Revealing…')
    expect(html).toContain('disabled=""')
  })
})
