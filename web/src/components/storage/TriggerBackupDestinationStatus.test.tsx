// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  CREATE_S3_SOURCE_PATH,
  DestinationsLoadError,
  DestinationsRefreshWarning,
  DestinationsSkeleton,
  NoDestinationsConfigured,
} from './TriggerBackupDestinationStatus'
import {
  DESTINATIONS_LOAD_FAILED_MESSAGE,
  DESTINATIONS_PERMISSION_EXPLANATION,
  DESTINATIONS_REFRESH_FAILED_MESSAGE,
  DESTINATIONS_UNAVAILABLE_EXPLANATION,
  classifyDestinationError,
} from './trigger-backup-state'

const noop = () => undefined

function render(element: React.ReactElement) {
  return renderToStaticMarkup(<MemoryRouter>{element}</MemoryRouter>)
}

describe('TriggerBackupDestinationStatus', () => {
  test('a 403 read shows the permission reason and Retry, never onboarding', () => {
    const markup = render(
      <DestinationsLoadError
        failure={classifyDestinationError({
          title: 'Forbidden',
          status: 403,
          detail: 'You do not have permission to read this resource.',
        })}
        onRetry={noop}
        isRetrying={false}
      />
    )

    expect(markup).toContain(DESTINATIONS_LOAD_FAILED_MESSAGE)
    expect(markup).toContain(DESTINATIONS_PERMISSION_EXPLANATION)
    expect(markup).toContain(
      'You do not have permission to read this resource.'
    )
    expect(markup).toContain('Retry')
    expect(markup).not.toContain('No S3 sources configured')
    expect(markup).not.toContain(CREATE_S3_SOURCE_PATH)
  })

  test('a network failure shows the availability reason without a detail line', () => {
    const markup = render(
      <DestinationsLoadError
        failure={classifyDestinationError(new TypeError('Failed to fetch'))}
        onRetry={noop}
        isRetrying={false}
      />
    )

    expect(markup).toContain(DESTINATIONS_LOAD_FAILED_MESSAGE)
    expect(markup).toContain(DESTINATIONS_UNAVAILABLE_EXPLANATION)
    expect(markup).not.toContain('Server response:')
    expect(markup).not.toContain('No S3 sources configured')
  })

  test('the Retry button is disabled while the retry is in flight', () => {
    const markup = render(
      <DestinationsLoadError
        failure={classifyDestinationError({ title: 'Oops', status: 500 })}
        onRetry={noop}
        isRetrying
      />
    )

    expect(markup).toContain('Retrying...')
    expect(markup).toMatch(/<button[^>]*disabled/)
  })

  test('a failed refresh shows a non-blocking warning with Retry', () => {
    const markup = render(
      <DestinationsRefreshWarning
        failure={classifyDestinationError({
          title: 'Service unavailable',
          status: 503,
          detail: 'The API is temporarily unavailable.',
        })}
        onRetry={noop}
        isRetrying={false}
      />
    )

    expect(markup).toContain(DESTINATIONS_REFRESH_FAILED_MESSAGE)
    expect(markup).toContain(DESTINATIONS_UNAVAILABLE_EXPLANATION)
    expect(markup).toContain('The API is temporarily unavailable.')
    expect(markup).toContain('Retry')
    expect(markup).not.toContain(CREATE_S3_SOURCE_PATH)
  })

  test('a successful empty list links to S3 source creation', () => {
    const markup = render(<NoDestinationsConfigured />)

    expect(markup).toContain('No S3 sources configured')
    expect(markup).toContain(`href="${CREATE_S3_SOURCE_PATH}"`)
    expect(markup).toContain('Create S3 source')
    expect(markup).not.toContain(DESTINATIONS_LOAD_FAILED_MESSAGE)
  })

  test('loading renders skeletons, not a spinner or a message', () => {
    const markup = render(<DestinationsSkeleton />)

    expect(markup).toContain('aria-busy="true"')
    expect(markup).toContain('animate-pulse')
    expect(markup).not.toContain('animate-spin')
    expect(markup).not.toContain('No S3 sources configured')
  })
})
