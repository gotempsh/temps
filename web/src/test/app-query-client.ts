// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { QueryClient } from '@tanstack/react-query'
import { announceMutationFailure } from '@/lib/mutation-error-toast'

/**
 * A query client with the app-wide mutation failure toast App.tsx installs,
 * for tests of pages that rely on it (`meta.errorTitle` plus the server's
 * reason) instead of a handler of their own.
 */
export function createAppQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: {
        retry: false,
        onError: (error, _variables, _result, context) =>
          announceMutationFailure(
            error as { title?: string | null; detail?: string | null },
            context
          ),
      },
    },
  })
}
