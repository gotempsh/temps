// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { isValidElement, type ReactElement, type ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  EnvironmentVariableRevealValue,
  EnvironmentVariableRevealView,
} from './EnvironmentVariableRevealValue'
import {
  createValueReveal,
  type ValueRevealState,
} from '@/lib/value-reveal-controller'

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

type ButtonProps = {
  'aria-label'?: string
  disabled?: boolean
  onClick?: () => void
}

function findButton(node: ReactNode): ReactElement<ButtonProps> | undefined {
  if (!isValidElement<{ children?: ReactNode } & ButtonProps>(node)) {
    return Array.isArray(node)
      ? node.map(findButton).find((found) => found !== undefined)
      : undefined
  }
  if (node.props.onClick) return node as ReactElement<ButtonProps>
  return findButton(node.props.children)
}

// The view is a pure function component, so calling it yields the element
// tree whose button `onClick` is exactly what a user click would invoke.
function clickReveal(controller: ReturnType<typeof createValueReveal>) {
  const state: ValueRevealState = controller.getState()
  const view = EnvironmentVariableRevealView({
    name: 'APP_URL',
    state,
    onReveal: () => void controller.reveal(),
    onHide: () => controller.hide(),
  })
  const button = findButton(view)
  expect(button).toBeDefined()
  button?.props.onClick?.()
  return button
}

describe('environment variable detail reveal interaction', () => {
  test('clicking Reveal fetches the value; clicking Hide masks it again', async () => {
    let calls = 0
    const controller = createValueReveal(async () => {
      calls++
      return 'https://app.example.test'
    })

    const reveal = clickReveal(controller)
    expect(reveal?.props['aria-label']).toBe('Reveal APP_URL')
    await Promise.resolve()
    await Promise.resolve()
    expect(controller.getState()).toEqual({
      phase: 'revealed',
      value: 'https://app.example.test',
    })
    expect(calls).toBe(1)

    const hide = clickReveal(controller)
    expect(hide?.props['aria-label']).toBe('Hide APP_URL')
    expect(controller.getState()).toEqual({ phase: 'hidden' })
  })

  test('a rejected reveal leaves the value masked and clickable again', async () => {
    let calls = 0
    const controller = createValueReveal(async () => {
      calls++
      throw new Error('403')
    })

    clickReveal(controller)
    await Promise.resolve()
    await Promise.resolve()
    expect(controller.getState()).toEqual({ phase: 'hidden' })

    const retry = clickReveal(controller)
    expect(retry?.props['aria-label']).toBe('Reveal APP_URL')
    expect(retry?.props.disabled).toBe(false)
    await Promise.resolve()
    expect(calls).toBe(2)
  })

  test('the variable changing while a reveal is pending drops the response', async () => {
    let resolve!: (value: string) => void
    const controller = createValueReveal(
      () => new Promise<string>((res) => (resolve = res))
    )
    clickReveal(controller)
    expect(controller.getState()).toEqual({ phase: 'revealing' })

    // A new version remounts the component, which disposes the controller.
    controller.dispose()
    resolve('previous-version-value')
    await Promise.resolve()
    await Promise.resolve()
    expect(controller.getState()).toEqual({ phase: 'hidden' })
  })

  test('the in-flight button is disabled', () => {
    const controller = createValueReveal(() => new Promise<string>(() => {}))
    clickReveal(controller)
    const pending = clickReveal(controller)
    expect(pending?.props.disabled).toBe(true)
  })
})
