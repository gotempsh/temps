// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import { signOutOfAppsMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { problemDetail } from '@/lib/api-problem'

/** How many sessions a sign-out ended, in words. */
export function signedOutMessage(revoked: number): string {
  if (revoked === 0) return 'You were not signed in to any app.'
  return revoked === 1
    ? 'Signed out of 1 app session.'
    : `Signed out of ${revoked} app sessions.`
}

/**
 * "Sign out of apps" (ADR 065 D8): ends every session this user has on
 * addresses behind the Temps sign-in, on every server that serves them.
 */
export function SignOutOfAppsCard() {
  const signOut = useMutation({
    ...signOutOfAppsMutation(),
    onSuccess: (data) => toast.success(signedOutMessage(data.revoked)),
    onError: (error) =>
      toast.error('Could not sign out of apps', {
        description: problemDetail(error, 'Try again in a moment.'),
      }),
  })
  return (
    <Card>
      <CardHeader>
        <CardTitle>Apps you signed in to</CardTitle>
        <CardDescription>
          Addresses for people who sign in send you through this console. Sign
          out of all of them here; you stay signed in to the console.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <Button
          variant="outline"
          onClick={() => signOut.mutate({})}
          disabled={signOut.isPending}
        >
          {signOut.isPending && (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          )}
          Sign out of apps
        </Button>
      </CardContent>
    </Card>
  )
}
