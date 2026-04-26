import { ProblemDetails } from '@/api/client/types.gen'

/**
 * Check if an error is a ProblemDetails error with expired token
 */
export function isExpiredTokenError(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false

  const problemDetails = error as Partial<ProblemDetails>

  // Check if the error has a type_url that indicates expired token
  if (
    problemDetails.type &&
    problemDetails.type.includes('/errors/expired_token')
  ) {
    return true
  }

  // Fallback: check title or detail for expired token mentions
  const title = problemDetails.title?.toLowerCase() || ''
  const detail = problemDetails.detail?.toLowerCase() || ''

  return (
    title.includes('expired') ||
    title.includes('token') ||
    detail.includes('expired') ||
    detail.includes('token expired')
  )
}

/**
 * Get a user-friendly error message for expired tokens
 */
export function getExpiredTokenMessage(
  error: unknown,
  connectionName?: string
): string {
  const problemDetails = error as Partial<ProblemDetails>

  // Use the detail from the error if available
  if (problemDetails.detail) {
    return problemDetails.detail
  }

  // Provide a default message
  const name = connectionName ? ` for ${connectionName}` : ''
  return `Your access token${name} has expired. Please update it to continue.`
}

/**
 * Extract ProblemDetails from an error.
 *
 * The hey-api openapi-ts client (with `throwOnError: true`) throws the parsed
 * response body directly, so `error` is typically the `ProblemDetails` object
 * itself. Older fetch-style errors may nest it under `body`. RFC 7807 uses
 * `type` (not `type_url`), so we identify a ProblemDetails by the presence
 * of a `title` string plus at least one of the canonical fields.
 */
export function extractProblemDetails(error: unknown): ProblemDetails | null {
  if (!error || typeof error !== 'object') return null

  const looksLikeProblem = (val: unknown): val is ProblemDetails => {
    if (!val || typeof val !== 'object') return false
    const v = val as Record<string, unknown>
    return (
      typeof v.title === 'string' &&
      ('detail' in v || 'type' in v || 'status' in v || 'extensions' in v)
    )
  }

  const err = error as Record<string, unknown>

  // Some clients nest the parsed body under `body`, `error`, or `data`.
  for (const key of ['body', 'error', 'data'] as const) {
    if (looksLikeProblem(err[key])) {
      return err[key] as ProblemDetails
    }
  }

  // Default case: the thrown value IS the ProblemDetails.
  if (looksLikeProblem(err)) {
    return err
  }

  return null
}

/**
 * Get a user-friendly error message from any error
 */
export function getErrorMessage(
  error: unknown,
  fallback = 'An error occurred'
): string {
  const problemDetails = extractProblemDetails(error)

  if (problemDetails) {
    // Check for expired token first
    if (isExpiredTokenError(problemDetails)) {
      return getExpiredTokenMessage(problemDetails)
    }

    // Return detail or title from problem details
    return problemDetails.detail || problemDetails.title || fallback
  }

  // Fallback to error message if available
  if (error && typeof error === 'object' && 'message' in error) {
    return (error as { message: string }).message
  }

  return fallback
}
