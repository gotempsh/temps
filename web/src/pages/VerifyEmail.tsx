// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { verifyEmailOptions } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, ArrowLeft, MailCheck } from 'lucide-react'
import { Link, useSearchParams } from 'react-router'

/**
 * Target of the verification email link (`{base_url}/auth/verify?token=...`,
 * see `send_verification_email` in temps-auth). Public: the link must work in
 * a browser that is not signed in.
 */
export const VerifyEmail = () => {
  usePageTitle('Verify email')
  const [searchParams] = useSearchParams()
  const token = searchParams.get('token') ?? ''

  // A link is single-use, so the request must run exactly once: no retries,
  // no refetch on focus, and the result is kept for the life of the page.
  const verification = useQuery({
    ...verifyEmailOptions({ query: { token } }),
    enabled: token.length > 0,
    retry: false,
    staleTime: Infinity,
    gcTime: Infinity,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })

  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-background p-4">
      <div className="w-full max-w-sm space-y-6">
        <div className="flex items-center justify-center gap-3">
          <img src="/svg/temps-icon.svg" alt="Temps logo" className="size-12" />
          <span className="text-2xl font-bold">Temps</span>
        </div>

        <Card className="w-full">
          <CardHeader>
            <CardTitle className="text-2xl">Verify your email</CardTitle>
            <CardDescription>
              Confirms that this address belongs to you.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <VerificationResult
              hasToken={token.length > 0}
              isLoading={verification.isLoading}
              isSuccess={verification.isSuccess}
              error={verification.error}
            />
            <Button variant="outline" className="w-full" asChild>
              <Link to="/">
                <ArrowLeft className="mr-2 h-4 w-4" />
                Go to Temps
              </Link>
            </Button>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}

function VerificationResult({
  hasToken,
  isLoading,
  isSuccess,
  error,
}: {
  hasToken: boolean
  isLoading: boolean
  isSuccess: boolean
  error: unknown
}) {
  if (!hasToken) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Missing verification token</AlertTitle>
        <AlertDescription>
          Open the link from the verification email again, or request a new one
          from your account page.
        </AlertDescription>
      </Alert>
    )
  }
  if (isLoading) {
    return (
      <div className="space-y-2" aria-busy="true">
        <Skeleton className="h-5 w-3/4" />
        <Skeleton className="h-4 w-full" />
      </div>
    )
  }
  if (isSuccess) {
    return (
      <Alert>
        <MailCheck className="h-4 w-4" />
        <AlertTitle>Email verified</AlertTitle>
        <AlertDescription>
          Your email address is verified. You can close this page or continue to
          Temps.
        </AlertDescription>
      </Alert>
    )
  }
  return (
    <Alert variant="destructive">
      <AlertCircle className="h-4 w-4" />
      <AlertTitle>This link can&apos;t be used</AlertTitle>
      <AlertDescription>
        {problemDetail(error, 'The verification link is invalid or expired.')}{' '}
        Request a new link from your account page, or ask an administrator to
        verify your email address.
      </AlertDescription>
    </Alert>
  )
}
