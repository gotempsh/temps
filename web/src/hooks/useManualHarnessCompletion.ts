// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSyncExternalStore } from 'react'
import { useAuth } from '@/contexts/AuthContext'

const CHANGED_EVENT = 'temps:harness-checklist-changed'

export function harnessCompletionKey(userId: number) {
  return `temps_getting_started_harness_completed:${userId}`
}

export function readHarnessCompletion(
  storage: Pick<Storage, 'getItem'>,
  userId: number
) {
  try {
    return storage.getItem(harnessCompletionKey(userId)) === 'true'
  } catch {
    return false
  }
}

function subscribe(onChange: () => void) {
  window.addEventListener('storage', onChange)
  window.addEventListener(CHANGED_EVENT, onChange)
  return () => {
    window.removeEventListener('storage', onChange)
    window.removeEventListener(CHANGED_EVENT, onChange)
  }
}

export function useManualHarnessCompletion() {
  const { user } = useAuth()
  const completed = useSyncExternalStore(
    subscribe,
    () => {
      try {
        return user
          ? readHarnessCompletion(window.localStorage, user.id)
          : false
      } catch {
        return false
      }
    },
    () => false
  )

  function markCompleted() {
    if (!user) throw new Error('Sign in before completing the checklist.')
    try {
      window.localStorage.setItem(harnessCompletionKey(user.id), 'true')
    } catch {
      throw new Error(
        'Your browser could not save checklist progress. Allow site storage, then retry.'
      )
    }
    window.dispatchEvent(new Event(CHANGED_EVENT))
  }

  return { completed, markCompleted }
}
