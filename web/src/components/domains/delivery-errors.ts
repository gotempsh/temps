/** Preserve the server's contextual Problem Details message in setup errors. */
export function deliveryError(error: unknown): string {
  if (error instanceof Error) return error.message
  if (error && typeof error === 'object') {
    if ('detail' in error && typeof error.detail === 'string')
      return error.detail
    if ('message' in error && typeof error.message === 'string')
      return error.message
    if ('title' in error && typeof error.title === 'string') return error.title
  }
  return 'The request could not be completed. Please try again.'
}

export function requireDeliveryData<T>(response: {
  data?: T
  error?: unknown
}): T {
  if (response.error) throw new Error(deliveryError(response.error))
  if (response.data === undefined)
    throw new Error(
      'The server returned no result. Please refresh and try again.'
    )
  return response.data
}
