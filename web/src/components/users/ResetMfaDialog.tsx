// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import { resetUserMfaMutation } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
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
import { AlertTriangle, Loader2, ShieldOff } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { resetMfaSuccessMessage } from './reset-mfa-message'

export interface ResetMfaTarget {
  id: number
  name: string
  email: string
}

interface ResetMfaDialogProps {
  /** User whose second factor is removed. `null` keeps the dialog closed. */
  user: ResetMfaTarget | null
  onClose: () => void
}

/**
 * Remove another user's second factor.
 *
 * The recovery path for a user who lost both their authenticator and their
 * recovery codes. Their password is kept: they sign in with it and enroll
 * again. When instance policy refuses that password sign-in for an admin
 * without MFA, the dialog stays open to say so instead of letting the
 * administrator find out from the user's failed login.
 */
export function ResetMfaDialog({ user, onClose }: ResetMfaDialogProps) {
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const [error, setError] = useState<string | null>(null)
  // Keyed by the user it applies to, so the warning is never shown under
  // another user's name.
  const [blockedUserId, setBlockedUserId] = useState<number | null>(null)

  const displayName = user?.name || user?.email || 'this user'

  const resetMfa = useMutation({
    ...resetUserMfaMutation(),
    onSuccess: (data, variables) => {
      setError(null)
      // Generated query keys are `[{ _id: 'listUsers', ...options }]`; match on
      // `_id` so every listUsers query (list page, detail page) refreshes.
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as { _id?: string } | undefined)?._id ===
          'listUsers',
      })
      toast.success(resetMfaSuccessMessage(displayName, data.sessions_revoked))
      if (data.password_sign_in_blocked) {
        setBlockedUserId(variables.path.user_id)
        return
      }
      finish()
    },
    onError: (err, variables) => {
      if (handleSensitiveActionError(err, () => resetMfa.mutate(variables)))
        return
      const problem = err as { detail?: string; message?: string }
      setError(
        problem.detail ||
          problem.message ||
          'MFA could not be reset. Please try again.'
      )
    },
  })

  function finish() {
    setError(null)
    setBlockedUserId(null)
    resetMfa.reset()
    onClose()
  }

  const close = () => {
    // Let a reset in flight report its outcome in this dialog.
    if (resetMfa.isPending) return
    finish()
  }

  const passwordSignInBlocked = user !== null && blockedUserId === user.id

  return (
    <>
      {verificationDialog}
      <Dialog open={user !== null} onOpenChange={(open) => !open && close()}>
        <DialogContent
          className="sm:max-w-lg"
          onInteractOutside={(e) => {
            if (resetMfa.isPending) e.preventDefault()
          }}
          onEscapeKeyDown={(e) => {
            if (resetMfa.isPending) e.preventDefault()
          }}
        >
          {passwordSignInBlocked ? (
            <>
              <DialogHeader>
                <DialogTitle>MFA removed for {displayName}</DialogTitle>
                <DialogDescription>
                  One more step before they can sign in with a password.
                </DialogDescription>
              </DialogHeader>
              <Alert>
                <AlertTriangle className="h-4 w-4" />
                <AlertDescription>
                  This instance requires MFA for admins (
                  <code className="font-mono text-xs">
                    require_mfa_for_admins
                  </code>
                  ), so {displayName} cannot sign in with a password until they
                  enroll again. They can sign in through SSO, or you can turn
                  the setting off through the settings API until they have
                  enrolled.
                </AlertDescription>
              </Alert>
              <DialogFooter>
                <Button onClick={finish}>Done</Button>
              </DialogFooter>
            </>
          ) : (
            <>
              <DialogHeader>
                <DialogTitle>Reset MFA for {displayName}?</DialogTitle>
                <DialogDescription>
                  Use this when {user?.email || displayName} has lost both their
                  authenticator and their recovery codes.
                </DialogDescription>
              </DialogHeader>
              <ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">
                <li>
                  Their authenticator app and recovery codes stop working
                  immediately.
                </li>
                <li>They are signed out of every browser session.</li>
                <li>
                  Their password does not change: they sign in with it and can
                  set up MFA again from their Account page.
                </li>
                <li>API keys they created keep working.</li>
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
                  disabled={resetMfa.isPending}
                >
                  Cancel
                </Button>
                <Button
                  variant="destructive"
                  onClick={() =>
                    user && resetMfa.mutate({ path: { user_id: user.id } })
                  }
                  disabled={resetMfa.isPending}
                >
                  {resetMfa.isPending ? (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  ) : (
                    <ShieldOff className="mr-2 h-4 w-4" />
                  )}
                  Reset MFA
                </Button>
              </DialogFooter>
            </>
          )}
        </DialogContent>
      </Dialog>
    </>
  )
}
