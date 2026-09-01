// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  changeRequiredPasswordMutation,
  verifyMfaChallengeMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { MfaSetupResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  PASSWORD_REQUIREMENTS,
  passwordRequirementResults,
  passwordSchema,
} from '@/lib/password-policy'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Check, Circle, Loader2 } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useLocation, useNavigate } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

const requiredPasswordChangeSchema = z
  .object({
    newPassword: passwordSchema,
    confirmPassword: z.string(),
  })
  .refine((data) => data.newPassword === data.confirmPassword, {
    message: 'Passwords do not match',
    path: ['confirmPassword'],
  })

type RequiredPasswordChangeFormData = z.infer<
  typeof requiredPasswordChangeSchema
>

const mfaCodeSchema = z.object({
  code: z.string().regex(/^\d{6}$/, 'Enter the 6-digit code'),
})

type MfaCodeFormData = z.infer<typeof mfaCodeSchema>

export function RequiredPasswordChange() {
  usePageTitle('Choose a new password')
  const navigate = useNavigate()
  const location = useLocation()
  const resumedSetup = (
    location.state as { mfaSetup?: MfaSetupResponse } | null
  )?.mfaSetup
  const [mfaSetup, setMfaSetup] = useState<MfaSetupResponse | null>(
    resumedSetup ?? null
  )
  const form = useForm<RequiredPasswordChangeFormData>({
    resolver: zodResolver(requiredPasswordChangeSchema),
    defaultValues: { newPassword: '', confirmPassword: '' },
  })
  const newPassword = form.watch('newPassword')
  const requirementResults = passwordRequirementResults(newPassword)
  const mfaForm = useForm<MfaCodeFormData>({
    resolver: zodResolver(mfaCodeSchema),
    defaultValues: { code: '' },
  })

  const changePassword = useMutation({
    ...changeRequiredPasswordMutation(),
    meta: { errorTitle: 'Password change failed' },
    onSuccess: (data) => {
      if (data.mfa_enrollment_required && data.mfa_setup) {
        setMfaSetup(data.mfa_setup)
        toast.success('Password changed. Set up MFA to continue.')
        return
      }
      toast.success('Password changed. Log in with your new password.')
      navigate('/', { replace: true })
    },
  })

  const verifyMfa = useMutation({
    ...verifyMfaChallengeMutation(),
    meta: { errorTitle: 'MFA verification failed' },
    onSuccess: () => {
      toast.success('MFA enabled successfully')
      navigate('/dashboard', { replace: true })
    },
  })

  const handleSubmit = async (data: RequiredPasswordChangeFormData) => {
    await changePassword.mutateAsync({
      body: { new_password: data.newPassword },
    })
  }

  if (mfaSetup) {
    return (
      <div className="flex min-h-dvh flex-col items-center justify-center bg-background p-4">
        <Card className="w-full max-w-lg">
          <CardHeader>
            <CardTitle>Secure your administrator account</CardTitle>
            <CardDescription>
              Scan this code with an authenticator app, save the recovery codes,
              then enter the current 6-digit code.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-5">
            <div className="flex justify-center">
              <img
                src={mfaSetup.qr_code}
                alt="QR code for multi-factor authentication"
                className="size-48 rounded-md border bg-white p-2"
              />
            </div>
            <div className="space-y-2 text-sm">
              <div className="font-medium">Manual setup key</div>
              <code className="block overflow-x-auto rounded-md bg-muted p-3 font-mono">
                {mfaSetup.secret_key}
              </code>
            </div>
            {mfaSetup.recovery_codes.length > 0 ? (
              <div className="space-y-2 text-sm">
                <div className="font-medium">Recovery codes</div>
                <p className="text-muted-foreground">
                  Store these somewhere safe. Each code can be used once.
                </p>
                <div className="grid grid-cols-2 gap-2 rounded-md bg-muted p-3 font-mono">
                  {mfaSetup.recovery_codes.map((code) => (
                    <code key={code}>{code}</code>
                  ))}
                </div>
              </div>
            ) : (
              <p className="text-sm text-destructive">
                Recovery codes could not be prepared. Log in again to restart
                MFA setup before enabling it.
              </p>
            )}
            <Form {...mfaForm}>
              <form
                onSubmit={mfaForm.handleSubmit((data) =>
                  verifyMfa.mutate({ body: data })
                )}
                className="space-y-4"
              >
                <FormField
                  control={mfaForm.control}
                  name="code"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Verification code</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          inputMode="numeric"
                          autoComplete="one-time-code"
                          maxLength={6}
                          placeholder="000000"
                        />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <Button
                  type="submit"
                  className="w-full"
                  disabled={
                    verifyMfa.isPending || mfaSetup.recovery_codes.length === 0
                  }
                >
                  {verifyMfa.isPending && (
                    <Loader2 className="size-4 animate-spin" />
                  )}
                  Verify and continue
                </Button>
              </form>
            </Form>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="flex min-h-dvh flex-col items-center justify-center bg-background p-4">
      <div className="w-full max-w-md space-y-6">
        <div className="flex items-center justify-center gap-3">
          <img src="/svg/temps-icon.svg" alt="Temps logo" className="size-12" />
          <div className="text-2xl font-semibold">Temps</div>
        </div>

        <Card>
          <CardHeader>
            <CardTitle className="text-balance text-2xl font-semibold tracking-tight">
              Choose a new password
            </CardTitle>
            <CardDescription className="text-pretty text-base sm:text-sm">
              Your administrator requires you to replace the temporary password
              before accessing Temps.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Form {...form}>
              <form
                onSubmit={form.handleSubmit(handleSubmit)}
                className="space-y-5"
              >
                <FormField
                  control={form.control}
                  name="newPassword"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>New password</FormLabel>
                      <FormControl>
                        <Input
                          type="password"
                          autoComplete="new-password"
                          placeholder="Enter a new password"
                          disabled={changePassword.isPending}
                          {...field}
                        />
                      </FormControl>
                      <FormDescription>
                        Meet every requirement below.
                      </FormDescription>
                      <FormMessage />
                      <ul role="list" className="grid gap-2 sm:grid-cols-2">
                        {PASSWORD_REQUIREMENTS.map((requirement, index) => {
                          const met = requirementResults[index]?.met ?? false
                          return (
                            <li
                              key={requirement.id}
                              className="flex items-baseline gap-2 text-base text-muted-foreground sm:text-sm"
                            >
                              {met ? (
                                <Check className="size-4 h-lh shrink-0 stroke-emerald-600" />
                              ) : (
                                <Circle className="size-4 h-lh shrink-0 stroke-muted-foreground" />
                              )}
                              <div>{requirement.label}</div>
                            </li>
                          )
                        })}
                      </ul>
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="confirmPassword"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Confirm new password</FormLabel>
                      <FormControl>
                        <Input
                          type="password"
                          autoComplete="new-password"
                          placeholder="Re-enter the new password"
                          disabled={changePassword.isPending}
                          {...field}
                        />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <Button
                  type="submit"
                  className="w-full"
                  disabled={changePassword.isPending}
                >
                  {changePassword.isPending && (
                    <Loader2 className="size-4 shrink-0 animate-spin" />
                  )}
                  {changePassword.isPending
                    ? 'Changing password…'
                    : 'Change password'}
                </Button>
              </form>
            </Form>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
