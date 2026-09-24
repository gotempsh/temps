// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import { resetUserPasswordMutation } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, KeyRound, Loader2 } from 'lucide-react'
import { useState } from 'react'

export interface ResetPasswordTarget {
  id: number
  name: string
  email: string
}

interface ResetPasswordDialogProps {
  /** User whose password is reset. `null` keeps the dialog closed. */
  user: ResetPasswordTarget | null
  onClose: () => void
}

/**
 * Reset another user's password to a server-generated temporary one.
 *
 * The recovery path for a user who lost their password on an instance
 * without outbound email. The temporary password is shown exactly once; the
 * user is signed out of every browser session and must choose a new password at next
 * sign-in.
 */
export function ResetPasswordDialog({
  user,
  onClose,
}: ResetPasswordDialogProps) {
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const [temporaryPassword, setTemporaryPassword] = useState<string | null>(
    null
  )
  const [error, setError] = useState<string | null>(null)

  const resetPassword = useMutation({
    ...resetUserPasswordMutation(),
    onSuccess: (data) => {
      setError(null)
      setTemporaryPassword(data.temporary_password)
      queryClient.invalidateQueries({ queryKey: ['listUsers'] })
    },
    onError: (err, variables) => {
      if (
        handleSensitiveActionError(err, () => resetPassword.mutate(variables))
      )
        return
      const problem = err as { detail?: string; message?: string }
      setError(
        problem.detail ||
          problem.message ||
          'The password could not be reset. Please try again.'
      )
    },
  })

  const close = () => {
    // Drop the credential from memory as soon as the dialog goes away.
    setTemporaryPassword(null)
    setError(null)
    resetPassword.reset()
    onClose()
  }

  const displayName = user?.name || user?.email || 'this user'

  return (
    <>
      {verificationDialog}
      <Dialog open={user !== null} onOpenChange={(open) => !open && close()}>
        <DialogContent
          className="sm:max-w-lg"
          // Losing the password to a stray click would force another reset.
          onInteractOutside={(e) => temporaryPassword && e.preventDefault()}
        >
          {temporaryPassword ? (
            <>
              <DialogHeader>
                <DialogTitle>Temporary password for {displayName}</DialogTitle>
                <DialogDescription>
                  Share it with {displayName} through a channel you trust. They
                  will be asked to choose a new password as soon as they sign
                  in.
                </DialogDescription>
              </DialogHeader>
              <div className="flex items-center gap-2 rounded-md border bg-muted/40 p-3">
                <code
                  className="flex-1 select-all break-all font-mono text-sm"
                  data-testid="temporary-password"
                >
                  {temporaryPassword}
                </code>
                <CopyButton
                  value={temporaryPassword}
                  label="Copy temporary password"
                />
              </div>
              <Alert>
                <AlertTriangle className="h-4 w-4" />
                <AlertDescription>
                  This password is shown only once and cannot be retrieved
                  later. Copy it now. If it gets lost, reset the password again.
                </AlertDescription>
              </Alert>
              <DialogFooter>
                <Button onClick={close}>Done</Button>
              </DialogFooter>
            </>
          ) : (
            <>
              <DialogHeader>
                <DialogTitle>Reset password for {displayName}?</DialogTitle>
                <DialogDescription>
                  A new temporary password will be generated for{' '}
                  {user?.email || displayName}.
                </DialogDescription>
              </DialogHeader>
              <ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">
                <li>Their current password stops working immediately.</li>
                <li>They are signed out of every browser session.</li>
                <li>
                  API keys they created keep working. Revoke them from their API
                  keys if you suspect they were compromised.
                </li>
                <li>
                  They must choose a new password the next time they sign in.
                </li>
                <li>Any pending password reset link is cancelled.</li>
              </ul>
              {error && (
                <Alert variant="destructive">
                  <AlertTriangle className="h-4 w-4" />
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}
              <DialogFooter>
                <Button
                  variant="outline"
                  onClick={close}
                  disabled={resetPassword.isPending}
                >
                  Cancel
                </Button>
                <Button
                  onClick={() =>
                    user && resetPassword.mutate({ path: { user_id: user.id } })
                  }
                  disabled={resetPassword.isPending}
                >
                  {resetPassword.isPending ? (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  ) : (
                    <KeyRound className="mr-2 h-4 w-4" />
                  )}
                  Reset password
                </Button>
              </DialogFooter>
            </>
          )}
        </DialogContent>
      </Dialog>
    </>
  )
}
