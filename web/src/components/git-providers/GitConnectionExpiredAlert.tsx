import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { AlertCircle } from 'lucide-react'
import { Link } from 'react-router-dom'

/**
 * Problem-details `type` the API returns when a git provider rejects the
 * stored credential (see `GitProviderError::AuthenticationFailed` →
 * `problem_new(UNAUTHORIZED)` in `temps-git/src/handlers/base.rs`).
 */
const AUTH_FAILED_PROBLEM_TYPE = 'authentication_failed'

/**
 * True when an error from any git-provider-backed query means "the stored
 * credential no longer works" rather than a transient provider fault.
 *
 * Matches on the RFC 7807 `type` when present. Older/edge paths that still
 * flatten a provider 401 into a generic API error are caught by the status
 * and message fallbacks, so a revoked token is never mistaken for an
 * unrelated failure — the user gets told to reconnect either way.
 */
export function isGitAuthError(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false
  const e = error as {
    type?: unknown
    status?: unknown
    detail?: unknown
    message?: unknown
  }
  if (typeof e.type === 'string' && e.type.includes(AUTH_FAILED_PROBLEM_TYPE)) {
    return true
  }
  if (e.status === 401 || e.status === 403) return true
  const text = [e.detail, e.message]
    .filter((v): v is string => typeof v === 'string')
    .join(' ')
  return /\b401\b|\b403\b|unauthorized|forbidden/i.test(text)
}

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
