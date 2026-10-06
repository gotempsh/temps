// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryObserver } from '@tanstack/react-query'
import {
  DESTINATIONS_PERMISSION_EXPLANATION,
  DESTINATIONS_UNAVAILABLE_EXPLANATION,
  canSubmitBackup,
  classifyDestinationError,
  deriveDestinationState,
  knownDestinations,
  selectionStatus,
  shouldRetryDestinationRead,
  type DestinationLike,
} from './trigger-backup-state'

interface Destination extends DestinationLike {
  name: string
}

const primary: Destination = { id: 1, name: 'primary-bucket' }
const archive: Destination = { id: 2, name: 'archive-bucket' }

const forbidden = {
  type: 'about:blank',
  title: 'Forbidden',
  status: 403,
  detail: 'You do not have permission to read this resource.',
}
const unavailable = {
  type: 'about:blank',
  title: 'Service unavailable',
  status: 500,
  detail: 'The API is temporarily unavailable. Retry when it is reachable.',
}
const networkFailure = new TypeError('Failed to fetch')

function failedInitialRead(error: unknown) {
  return deriveDestinationState<Destination>({
    data: undefined,
    error,
    isPending: false,
    isError: true,
  })
}

describe('classifyDestinationError', () => {
  test('403 problem is a permission failure carrying the server detail', () => {
    expect(classifyDestinationError(forbidden)).toEqual({
      reason: 'permission',
      status: 403,
      explanation: DESTINATIONS_PERMISSION_EXPLANATION,
      detail: forbidden.detail,
    })
  })

  test('401 problem is a permission failure', () => {
    expect(
      classifyDestinationError({ title: 'Unauthorized', status: 401 }).reason
    ).toBe('permission')
  })

  test('a Forbidden problem without a status is still a permission failure', () => {
    const failure = classifyDestinationError({
      title: 'Forbidden',
      detail: 'You do not have permission to read this resource.',
    })
    expect(failure.reason).toBe('permission')
    expect(failure.status).toBeUndefined()
    expect(failure.detail).toBe(
      'You do not have permission to read this resource.'
    )
  })

  test('5xx problem is an availability failure carrying the server detail', () => {
    expect(classifyDestinationError(unavailable)).toEqual({
      reason: 'unavailable',
      status: 500,
      explanation: DESTINATIONS_UNAVAILABLE_EXPLANATION,
      detail: unavailable.detail,
    })
  })

  test('a fetch TypeError is an availability failure without detail', () => {
    expect(classifyDestinationError(networkFailure)).toEqual({
      reason: 'unavailable',
      explanation: DESTINATIONS_UNAVAILABLE_EXPLANATION,
    })
  })

  test('a network message on a plain error is an availability failure', () => {
    expect(
      classifyDestinationError({ message: 'NetworkError when attempting' })
        .reason
    ).toBe('unavailable')
  })

  test('a non-JSON error body is an availability failure', () => {
    expect(classifyDestinationError('<html>Bad Gateway</html>').reason).toBe(
      'unavailable'
    )
  })

  test('other problems keep only the server detail', () => {
    expect(
      classifyDestinationError({
        title: 'Bad Request',
        status: 400,
        detail: 'Invalid query parameter',
      })
    ).toEqual({
      reason: 'unknown',
      status: 400,
      detail: 'Invalid query parameter',
    })
  })

  test('a blank detail is not reported', () => {
    expect(
      classifyDestinationError({ title: 'Forbidden', status: 403, detail: ' ' })
        .detail
    ).toBeUndefined()
  })
})

describe('shouldRetryDestinationRead', () => {
  test('never retries a permission refusal', () => {
    expect(shouldRetryDestinationRead(0, forbidden)).toBe(false)
  })

  test('retries server and network failures up to three times', () => {
    expect(shouldRetryDestinationRead(0, unavailable)).toBe(true)
    expect(shouldRetryDestinationRead(2, networkFailure)).toBe(true)
    expect(shouldRetryDestinationRead(3, networkFailure)).toBe(false)
  })
})

describe('deriveDestinationState', () => {
  test('loading: availability unknown, submission blocked', () => {
    const state = deriveDestinationState<Destination>({
      data: undefined,
      error: null,
      isPending: true,
      isError: false,
    })
    expect(state).toEqual({ kind: 'loading' })
    expect(knownDestinations(state)).toBeUndefined()
    expect(canSubmitBackup(state, primary.id)).toBe(false)
  })

  test('failed initial read with 403 is an error, not an empty list', () => {
    const state = failedInitialRead(forbidden)
    expect(state.kind).toBe('error')
    if (state.kind !== 'error') throw new Error('expected error state')
    expect(state.failure.reason).toBe('permission')
    expect(canSubmitBackup(state, primary.id)).toBe(false)
  })

  test('failed initial read with 500 is an error, not an empty list', () => {
    const state = failedInitialRead(unavailable)
    expect(state.kind).toBe('error')
    if (state.kind !== 'error') throw new Error('expected error state')
    expect(state.failure.reason).toBe('unavailable')
    expect(state.failure.detail).toBe(unavailable.detail)
    expect(canSubmitBackup(state, primary.id)).toBe(false)
  })

  test('failed initial read with a network error is an error, not an empty list', () => {
    const state = failedInitialRead(networkFailure)
    expect(state.kind).toBe('error')
    if (state.kind !== 'error') throw new Error('expected error state')
    expect(state.failure.reason).toBe('unavailable')
    expect(canSubmitBackup(state, primary.id)).toBe(false)
  })

  test('failed refresh with cached destinations keeps the list and the selection', () => {
    const state = deriveDestinationState<Destination>({
      data: [primary, archive],
      error: unavailable,
      isPending: false,
      isError: true,
    })
    expect(state.kind).toBe('stale')
    if (state.kind !== 'stale') throw new Error('expected stale state')
    expect(state.destinations).toEqual([primary, archive])
    expect(state.failure.reason).toBe('unavailable')
    expect(selectionStatus(state.destinations, archive.id)).toBe('valid')
    expect(canSubmitBackup(state, archive.id)).toBe(true)
    // A selection the cached list does not contain is still blocked.
    expect(canSubmitBackup(state, 99)).toBe(false)
  })

  test('failed refresh over a cached empty list is an error, not onboarding', () => {
    const state = deriveDestinationState<Destination>({
      data: [],
      error: forbidden,
      isPending: false,
      isError: true,
    })
    expect(state.kind).toBe('error')
    expect(canSubmitBackup(state, undefined)).toBe(false)
  })

  test('successful empty list is the only empty state, and blocks submission', () => {
    const state = deriveDestinationState<Destination>({
      data: [],
      error: null,
      isPending: false,
      isError: false,
    })
    expect(state).toEqual({ kind: 'empty' })
    expect(canSubmitBackup(state, undefined)).toBe(false)
    expect(canSubmitBackup(state, primary.id)).toBe(false)
  })

  test('successful list is ready and submits a listed selection', () => {
    const state = deriveDestinationState<Destination>({
      data: [primary],
      error: null,
      isPending: false,
      isError: false,
    })
    expect(state).toEqual({ kind: 'ready', destinations: [primary] })
    expect(canSubmitBackup(state, primary.id)).toBe(true)
  })

  test('ready list blocks a selection that is no longer listed', () => {
    const state = deriveDestinationState<Destination>({
      data: [archive],
      error: null,
      isPending: false,
      isError: false,
    })
    expect(selectionStatus([archive], primary.id)).toBe('missing')
    expect(canSubmitBackup(state, primary.id)).toBe(false)
  })

  test('ready list blocks submission without a selection', () => {
    const state = deriveDestinationState<Destination>({
      data: [primary],
      error: null,
      isPending: false,
      isError: false,
    })
    expect(selectionStatus([primary], undefined)).toBe('none')
    expect(canSubmitBackup(state, undefined)).toBe(false)
  })
})

describe('deriveDestinationState with a real React Query observer', () => {
  async function settled(observer: QueryObserver<Destination[]>) {
    for (let i = 0; i < 100; i += 1) {
      const result = observer.getCurrentResult()
      if (!result.isFetching && !result.isPending) return
      if (!result.isFetching && result.isError) return
      await new Promise((resolve) => setTimeout(resolve, 1))
    }
    throw new Error('query did not settle')
  }

  function observe(queryFn: () => Promise<Destination[]>) {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: Infinity } },
    })
    const observer = new QueryObserver<Destination[]>(client, {
      queryKey: ['backup-destinations'],
      queryFn,
    })
    const unsubscribe = observer.subscribe(() => undefined)
    const snapshot = () => {
      const result = observer.getCurrentResult()
      return deriveDestinationState<Destination>({
        data: result.data,
        error: result.error,
        isPending: result.isPending,
        isError: result.isError,
      })
    }
    return { observer, snapshot, unsubscribe }
  }

  test('retry after a failed initial read recovers to the loaded list', async () => {
    let attempt = 0
    const { observer, snapshot, unsubscribe } = observe(async () => {
      attempt += 1
      if (attempt === 1) throw forbidden
      return [primary]
    })

    expect(snapshot().kind).toBe('loading')
    await settled(observer)
    expect(snapshot().kind).toBe('error')

    // What the dialog's Retry button does.
    await observer.refetch()
    const recovered = snapshot()
    expect(recovered).toEqual({ kind: 'ready', destinations: [primary] })
    expect(canSubmitBackup(recovered, primary.id)).toBe(true)
    unsubscribe()
  })

  test('a failed refetch keeps cached destinations as stale', async () => {
    let attempt = 0
    const { observer, snapshot, unsubscribe } = observe(async () => {
      attempt += 1
      if (attempt === 1) return [primary, archive]
      throw unavailable
    })

    await settled(observer)
    expect(snapshot().kind).toBe('ready')

    await observer.refetch()
    const state = snapshot()
    expect(state.kind).toBe('stale')
    expect(knownDestinations(state)).toEqual([primary, archive])
    expect(canSubmitBackup(state, archive.id)).toBe(true)
    unsubscribe()
  })
})
