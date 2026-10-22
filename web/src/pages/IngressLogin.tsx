// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation } from '@tanstack/react-query'
import { useMemo } from 'react'
import { useLocation } from 'react-router'
import { toast } from 'sonner'
import { AlertTriangle, Globe, Loader2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useAuth } from '@/contexts/AuthContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import {
  createIngressLoginGrant,
  parseIngressLoginRequest,
} from '@/lib/ingress-login'

/**
 * Sign-in hand-off for an environment that requires a Temps login
 * (ADR-055 §11).
 *
 * The proxy sends a visitor here with a signed request naming the site. The
 * protected layout has already made sure they are signed in (bouncing
 * through `/login` and back via `captureReturnTo()`), so this page only asks
 * them to confirm the site, then sends them back to it with a short-lived
 * grant that the proxy exchanges for a session cookie.
 */
export function IngressLogin() {
  usePageTitle('Open site')
  const location = useLocation()
  const { user } = useAuth()
  const request = useMemo(
    () => parseIngressLoginRequest(location.search),
    [location.search]
  )

  const grant = useMutation({
    mutationFn: createIngressLoginGrant,
    meta: { errorTitle: 'Could not open the site' },
    onSuccess: (data) => {
      window.location.assign(data.redirect_url)
    },
    onError: (error) => {
      toast.error(problemDetail(error, 'Could not open the site'))
    },
  })

  return (
    <div className="fixed inset-0 z-50 flex min-h-dvh items-center justify-center overflow-y-auto bg-background p-4 sm:p-8">
      <Card className="w-full max-w-lg shadow-lg">
        <CardHeader className="space-y-4 pb-5 text-center">
          <div className="mx-auto flex h-12 w-12 items-center justify-center rounded-full border bg-muted/50">
            <Globe className="h-6 w-6" />
          </div>
          <CardTitle className="text-2xl">Open this site?</CardTitle>
          <CardDescription>
            This site only admits people signed in to Temps. Continue to open it
            as {user?.email ?? 'your account'}.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {request ? (
            <div className="rounded-lg border bg-muted/30 px-4 py-5 text-center">
              <div className="text-xs uppercase tracking-wider text-muted-foreground">
                Site
              </div>
              <div className="mt-1 break-all font-mono text-lg font-semibold">
                {request.host}
              </div>
            </div>
          ) : (
            <ErrorRow detail="This sign-in link is incomplete. Open the site again to get a new one." />
          )}
          {grant.isError ? (
            <ErrorRow
              detail={problemDetail(grant.error, 'Could not open the site')}
            />
          ) : null}
        </CardContent>
        <CardFooter>
          <Button
            type="button"
            className="w-full"
            disabled={!request || grant.isPending || grant.isSuccess}
            onClick={() => {
              if (request) grant.mutate(request)
            }}
          >
            {grant.isPending || grant.isSuccess ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : null}
            Continue to {request?.host ?? 'site'}
          </Button>
        </CardFooter>
      </Card>
    </div>
  )
}

function ErrorRow({ detail }: { detail: string }) {
  return (
    <div className="flex items-start gap-3 rounded-lg border border-destructive/40 bg-destructive/5 p-4 text-sm">
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
      <p>{detail}</p>
    </div>
  )
}
