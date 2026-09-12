// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useSearchParams } from 'react-router'
import { authorizeWorkspacePreview } from '@/api/client'
import { ProtectedLayout } from '@/components/layout/ProtectedLayout'
import { Button } from '@/components/ui/button'
import { useAuth } from '@/contexts/AuthContext'
import {
  previewErrorMessage,
  previewAccessRequest,
} from '@/components/ai-first/application-preview'

/** Keep the console login/session on its own origin, never on an app host. */
export default function SandboxPreviewAccess() {
  const { user } = useAuth()
  return (
    <>
      {!user && (
        <h1 className="px-6 pt-6 text-center text-lg font-medium">
          Sign in to Temps to view this preview
        </h1>
      )}
      <ProtectedLayout>
        <AuthorizePreview />
      </ProtectedLayout>
    </>
  )
}

function AuthorizePreview() {
  const [params] = useSearchParams()
  const request = previewAccessRequest(params)
  const authorization = useQuery({
    queryKey: ['workspace-preview-authorization', request],
    queryFn: async () => {
      if (!request) throw new Error('Invalid sandbox preview address.')
      const { data } = await authorizeWorkspacePreview({
        body: request,
        throwOnError: true,
      })
      return data
    },
    enabled: request !== null,
    retry: false,
    gcTime: 0,
    staleTime: 0,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })
  useEffect(() => {
    if (authorization.data) window.location.replace(authorization.data.url)
  }, [authorization.data])
  const error = !request
    ? 'Invalid sandbox preview address.'
    : authorization.error
      ? previewErrorMessage(authorization.error)
      : null
  return (
    <main className="mx-auto flex min-h-svh max-w-lg flex-col justify-center gap-4 px-6">
      <picture>
        <source
          media="(prefers-color-scheme: dark)"
          srcSet="/svg/temps-logo-dark.svg"
        />
        <img
          src="/svg/temps-logo-light.svg"
          alt="Temps"
          width="185"
          height="80"
        />
      </picture>
      <h1 className="text-xl font-medium">
        {error ? 'Could not open preview' : 'Opening your preview…'}
      </h1>
      <p className="text-sm text-muted-foreground">
        {error ??
          'Checking your current workspace access and renewing the preview session.'}
      </p>
      {error && (
        <div className="flex gap-3">
          {request && (
            <Button
              disabled={authorization.isFetching}
              onClick={() => void authorization.refetch()}
            >
              Retry
            </Button>
          )}
          <Button asChild variant="outline">
            <a href="/workspaces">Back to workspaces</a>
          </Button>
        </div>
      )}
    </main>
  )
}
