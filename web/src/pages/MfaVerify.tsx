// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import type { ProblemDetails } from '@/api/client'
import { verifyMfaChallengeMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { useNavigate, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { useAuth } from '@/contexts/AuthContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { isMfaChallengeEnded } from '@/lib/mfaChallengeProblem'
import { announceMutationFailure } from '@/lib/mutation-error-toast'
import {
  consumeReturnTo,
  goToReturnTo,
  rememberReturnTo,
} from '@/lib/return-to'
import * as z from 'zod'

const formSchema = z.object({
  // A 6-digit authenticator code, or a recovery code (current codes are
  // `XXXXX-XXXXX`; codes issued by older versions are 6 digits).
  code: z
    .string()
    .trim()
    .min(6, 'Enter a 6-digit code or a recovery code')
    .max(32, 'Enter a 6-digit code or a recovery code'),
})

type FormValues = z.infer<typeof formSchema>

export const MfaVerify = () => {
  usePageTitle('Verify MFA')
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const queryClient = useQueryClient()
  const { refetch } = useAuth()

  // The OIDC callback handler may set `?return_to=` when SSO ran on
  // a different tab / private window than the one that captured the
  // deep-link target in sessionStorage. Query param wins because
  // the server signed off on it (it round-tripped through
  // sanitize_return_to in oidc_service.rs); sessionStorage is only
  // a fallback for the same-tab login flow.
  const queryReturnTo = searchParams.get('return_to')

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      code: '',
    },
  })

  const verifyMfaChallenge = useMutation({
    ...verifyMfaChallengeMutation(),
    meta: {
      errorTitle: 'MFA verification failed',
    },
    onError: (error, _variables, _onMutateResult, context) => {
      // A per-mutation onError replaces the app-wide default one, so announce
      // the failure here: a wrong code, a lockout and an ended sign-in all
      // need the server's reason on screen.
      announceMutationFailure(error as unknown as ProblemDetails, context)
      // The sign-in attempt is over (too many wrong codes, or it expired):
      // no further code can succeed, so return to the password step.
      if (isMfaChallengeEnded(error)) {
        // An SSO sign-in may carry its only destination in `?return_to=`;
        // keep it for the next sign-in instead of dropping it here.
        rememberReturnTo(queryReturnTo)
        form.reset()
        navigate('/', { replace: true })
      }
    },
    onSuccess: async () => {
      toast.success('MFA verified successfully')
      // Invalidate and refetch user data
      await queryClient.invalidateQueries({ queryKey: ['getCurrentUser'] })
      await refetch()
      // Navigate using React Router, honoring the pre-login return path if any.
      // Server-supplied query param (set by the OIDC callback) wins over
      // the sessionStorage fallback because it survives tab switches.
      // Same allow-list as `validate_return_to` on the server: must
      // start with `/`, must not be scheme-relative (`//`), and must
      // not contain `\` (Chrome/Edge normalise `/\evil.com` to
      // `//evil.com` and treat it as scheme-relative → open redirect).
      const isSafeRelativePath = (p: string | null): p is string =>
        !!p && p.startsWith('/') && !p.startsWith('//') && !p.includes('\\')
      const target = isSafeRelativePath(queryReturnTo)
        ? queryReturnTo
        : consumeReturnTo('/dashboard')
      goToReturnTo(navigate, target)
    },
  })
  const onSubmit = async (data: FormValues) => {
    verifyMfaChallenge.mutate({
      body: {
        code: data.code,
      },
    })
  }

  return (
    <div className="flex min-h-screen flex-col items-center justify-center bg-background">
      <div className="mx-auto flex w-full flex-col justify-center space-y-6 sm:w-[350px]">
        <div className="flex flex-col space-y-2 text-center">
          <h1 className="text-2xl font-semibold tracking-tight">
            Two-factor authentication
          </h1>
          <p className="text-sm text-muted-foreground">
            Enter the 6-digit code from your authenticator app, or one of
            your recovery codes
          </p>
        </div>

        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
            <FormField
              control={form.control}
              name="code"
              render={({ field }) => (
                <FormItem>
                  <FormControl>
                    <Input
                      {...field}
                      placeholder="000000"
                      type="text"
                      maxLength={32}
                      autoCapitalize="characters"
                      spellCheck={false}
                      className="text-center text-lg tracking-widest"
                      autoComplete="one-time-code"
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <Button
              type="submit"
              className="w-full"
              disabled={verifyMfaChallenge.isPending}
            >
              {verifyMfaChallenge.isPending ? 'Verifying...' : 'Verify'}
            </Button>
          </form>
        </Form>

        <p className="px-8 text-center text-sm text-muted-foreground">
          Didn&apos;t receive a code?{' '}
          <button
            onClick={() => {
              navigate('/')
            }}
            className="underline underline-offset-4 hover:text-primary"
          >
            Go back to login
          </button>
        </p>
      </div>
    </div>
  )
}
