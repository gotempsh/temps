// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  QueryClient,
  QueryClientProvider,
  useMutation,
} from '@tanstack/react-query'
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { installBrowserWindow } from '@/test/dom'
import { useComponentAddition } from './use-adding-component'

installBrowserWindow()
afterEach(cleanup)

/** Two forms on one client, each with an editor that can start a lookup. */
function useTwoForms(lookup: () => Promise<void>) {
  const left = useComponentAddition()
  const right = useComponentAddition()
  const leftLookup = useMutation({
    mutationKey: left.mutationKey,
    mutationFn: lookup,
  })
  return { left, right, leftLookup }
}

test('a lookup holds only the save of the form that started it', async () => {
  let release: () => void = () => {}
  const held = new Promise<void>((resolve) => (release = resolve))
  const client = new QueryClient()
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  const { result } = renderHook(() => useTwoForms(() => held), { wrapper })

  act(() => result.current.leftLookup.mutate())
  await waitFor(() => expect(result.current.left.adding).toBe(true))
  expect(result.current.right.adding).toBe(false)

  release()
  await waitFor(() => expect(result.current.left.adding).toBe(false))
})
