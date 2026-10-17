// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, spyOn, test } from 'bun:test'
import { MutationCache, QueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  announceMutationFailure,
  getErrorTitle,
  type MutationErrorContext,
} from './mutation-error-toast'

afterEach(() => {
  spyOn(toast, 'error').mockRestore()
})

describe('getErrorTitle', () => {
  test('prefers the authored meta.errorTitle over the server title', () => {
    expect(
      getErrorTitle(
        { meta: { errorTitle: 'Could not add the domain' } },
        'Hostname Unavailable'
      )
    ).toBe('Could not add the domain')
  })

  test('derives a title from the mutation key when there is no meta title', () => {
    expect(
      getErrorTitle({ mutationKey: ['createProject'] }, 'Server title')
    ).toBe('Failed to create project')
  })

  test('falls back to the server title, then to undefined', () => {
    expect(getErrorTitle({ meta: {} }, 'Server title')).toBe('Server title')
    expect(getErrorTitle(undefined)).toBeUndefined()
  })

  test('ignores a non-string title or key instead of throwing', () => {
    const context = {
      meta: { errorTitle: 42 },
      mutationKey: [{ scope: 'x' }],
    } as MutationErrorContext
    expect(getErrorTitle(context, 'Server title')).toBe('Server title')
  })
})

describe('announceMutationFailure', () => {
  test('shows the authored title with the server reason as description', () => {
    const error = spyOn(toast, 'error').mockImplementation(() => 'id')
    announceMutationFailure(
      { title: 'Hostname Unavailable', detail: "'a.example.test' is in use" },
      { meta: { errorTitle: 'Could not add the domain' } }
    )
    expect(error).toHaveBeenCalledWith('Could not add the domain', {
      description: "'a.example.test' is in use",
    })
  })

  test('shows the server title when the mutation authored none', () => {
    const error = spyOn(toast, 'error').mockImplementation(() => 'id')
    announceMutationFailure(
      { title: 'Too Many Domain Operations', detail: 'Try again in an hour' },
      undefined
    )
    expect(error).toHaveBeenCalledWith('Too Many Domain Operations', {
      description: 'Try again in an hour',
    })
  })

  test('still shows a message when the body carries neither title nor detail', () => {
    const error = spyOn(toast, 'error').mockImplementation(() => 'id')
    announceMutationFailure({}, undefined)
    expect(error).toHaveBeenCalledWith('An error occurred', undefined)
  })

  test('keeps the server reason when only the authored title is known', () => {
    const error = spyOn(toast, 'error').mockImplementation(() => 'id')
    announceMutationFailure(
      { detail: 'upstream refused' },
      { meta: { errorTitle: 'Failed to renew certificate' } }
    )
    expect(error).toHaveBeenCalledWith('Failed to renew certificate', {
      description: 'upstream refused',
    })
  })
})

// react-query passes `meta` in the fourth `onError` argument, not the third;
// reading the third is what made every authored title dead copy. Run a real
// mutation through a real client so a library change cannot silently bring it
// back.
describe('with a real QueryClient', () => {
  function clientWithDefaultHandler() {
    return new QueryClient({
      mutationCache: new MutationCache(),
      defaultOptions: {
        mutations: {
          retry: false,
          onError: (error, _variables, _onMutateResult, context) =>
            announceMutationFailure(
              error as { title?: string; detail?: string },
              context
            ),
        },
      },
    })
  }

  test('a failed mutation with meta.errorTitle announces that title and the server detail', async () => {
    const error = spyOn(toast, 'error').mockImplementation(() => 'id')
    const client = clientWithDefaultHandler()
    const mutation = client.getMutationCache().build(client, {
      meta: { errorTitle: 'Could not verify status.example.test yet' },
      mutationFn: async () => {
        throw {
          title: 'Hostname Unavailable',
          detail: 'publish the page first',
        }
      },
    })
    await mutation.execute(undefined).catch(() => undefined)
    expect(error).toHaveBeenCalledWith(
      'Could not verify status.example.test yet',
      { description: 'publish the page first' }
    )
  })
})
