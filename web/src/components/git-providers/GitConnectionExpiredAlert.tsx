// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { AlertCircle } from 'lucide-react'
import { Link } from 'react-router'

export { isGitAuthError } from '@/lib/git-auth-error'

/**
 * Actionable state for a git connection whose credential has stopped working.
 *
 * Self-hosted users have no support channel, so a raw "HTTP 401" tells them
 * nothing they can act on. This names the cause, says what breaks, and links
 * to the page where the connection is re-authorized.
 */
export function GitConnectionExpiredAlert({
  /** Shown in the message so the user knows which step failed. */
  operation = 'read this repository',
  className,
}: {
  operation?: string
  className?: string
}) {
  return (
    <Alert variant="destructive" className={className}>
      <AlertCircle className="size-4" />
      <AlertDescription className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <span>
          Your git provider connection is no longer authorized, so Temps could
          not {operation}. Reconnect the provider to restore access.
        </span>
        <Button
          variant="outline"
          size="sm"
          className="shrink-0 bg-transparent"
          asChild
        >
          <Link to="/git-providers">Reconnect provider</Link>
        </Button>
      </AlertDescription>
    </Alert>
  )
}
