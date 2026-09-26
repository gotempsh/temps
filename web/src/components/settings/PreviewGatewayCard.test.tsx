// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { PreviewGatewayErrorAlert } from './PreviewGatewayCard'
import { gatewayErrorAfterSuccessfulAction } from './preview-gateway-errors'

describe('PreviewGatewayErrorAlert', () => {
  test('renders a persistent accessible action error', () => {
    const markup = renderToStaticMarkup(
      <PreviewGatewayErrorAlert
        error={{
          action: 'restart',
          title: 'Failed to restart preview gateway',
          message:
            'Host port 18090 is already in use. Change the configured host port below.',
        }}
        onDismiss={() => {}}
      />
    )

    expect(markup).toContain('role="alert"')
    expect(markup).toContain('aria-live="assertive"')
    expect(markup).toContain('Failed to restart preview gateway')
    expect(markup).toContain('Host port 18090 is already in use')
    expect(markup).toContain('aria-label="Dismiss gateway error"')
  })

  test('keeps an action error until that action succeeds or it is dismissed', () => {
    const restartError = {
      action: 'restart' as const,
      title: 'Failed to restart preview gateway',
      message: 'Host port 18090 is already in use.',
    }

    expect(gatewayErrorAfterSuccessfulAction(restartError, 'refresh')).toBe(
      restartError
    )
    expect(gatewayErrorAfterSuccessfulAction(restartError, 'logs')).toBe(
      restartError
    )
    expect(
      gatewayErrorAfterSuccessfulAction(restartError, 'restart')
    ).toBeNull()
  })
})
