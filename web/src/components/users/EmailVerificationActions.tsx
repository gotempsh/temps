// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  emailStatusOptions,
  getCurrentUserOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  markUserEmailVerified,
  NO_EMAIL_PROVIDER_MESSAGE,
  resendEmailVerification,
} from '@/lib/email-verification'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2, MailCheck, MailWarning, Send } from 'lucide-react'
import { toast } from 'sonner'

/**
 * Lets the signed-in user ask for a fresh verification link for their own
 * address. Without an email provider there is nothing to send, so it says
 * that and points the user at an administrator instead of offering a button
 * that can only fail.
 */
export function ResendVerificationEmail() {
  const emailStatus = useQuery(emailStatusOptions())

  const resend = useMutation({
    mutationFn: resendEmailVerification,
    meta: { errorTitle: "Couldn't send the verification email" },
    onSuccess: (data) => {
      toast.success(`Verification link sent to ${data.email}`, {
        description: 'Open the link in that email to verify your address.',
      })
    },
  })

  if (emailStatus.isLoading) {
    return <Skeleton className="h-8 w-56" />
  }

  if (emailStatus.data && !emailStatus.data.email_configured) {
    return (
      <p className="flex items-start gap-1.5 text-sm text-muted-foreground">
        <MailWarning
          aria-hidden="true"
          className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400"
        />
        <span>{NO_EMAIL_PROVIDER_MESSAGE}</span>
      </p>
    )
  }

  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      disabled={resend.isPending}
      onClick={() => resend.mutate()}
    >
      {resend.isPending ? (
        <Loader2 className="mr-2 h-4 w-4 animate-spin" />
      ) : (
        <Send className="mr-2 h-4 w-4" />
      )}
      Resend verification email
    </Button>
  )
}

interface MarkEmailVerifiedButtonProps {
  userId: number
  email: string
  disabled?: boolean
  disabledReason?: string
}

/**
 * Administrator action: vouch for a user's current address. SSO sign-in is
 * only linked onto an account whose email is verified, so this is how an
 * administrator unblocks a user on an instance without an email provider.
 */
export function MarkEmailVerifiedButton({
  userId,
  email,
  disabled,
  disabledReason,
}: MarkEmailVerifiedButtonProps) {
  const queryClient = useQueryClient()

  const markVerified = useMutation({
    mutationFn: () => markUserEmailVerified(userId),
    meta: { errorTitle: "Couldn't mark the email as verified" },
    onSuccess: () => {
      toast.success(`${email} marked as verified`)
      // Generated query keys are `[{ _id: 'listUsers', ...options }]`; match
      // on `_id` so every users query (list page, detail page) refreshes.
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as { _id?: string } | undefined)?._id ===
          'listUsers',
      })
      queryClient.invalidateQueries({
        queryKey: getCurrentUserOptions().queryKey,
      })
    },
  })

  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      disabled={disabled || markVerified.isPending}
      title={disabled ? disabledReason : undefined}
      onClick={() => markVerified.mutate()}
    >
      {markVerified.isPending ? (
        <Loader2 className="mr-2 h-4 w-4 animate-spin" />
      ) : (
        <MailCheck className="mr-2 h-4 w-4" />
      )}
      Mark email as verified
    </Button>
  )
}
