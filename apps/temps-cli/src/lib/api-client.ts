import { client } from '../api/client.gen.js'
import { config, credentials } from '../config/store.js'

/**
 * Setup the API client with the correct base URL and auth headers
 */
export async function setupClient(): Promise<void> {
  const apiUrl = config.get('apiUrl')

  client.setConfig({
    baseUrl: apiUrl,
  })

  // Add auth header interceptor
  client.interceptors.request.use(async (request: Request) => {
    const apiKey = await credentials.getApiKey()
    if (apiKey) {
      request.headers.set('Authorization', `Bearer ${apiKey}`)
    }
    return request
  })
}

/**
 * Extract error message from API error response
 */
export function getErrorMessage(error: unknown): string {
  if (!error) return 'Unknown error'

  // Handle object with message property
  if (typeof error === 'object' && error !== null) {
    if ('message' in error && typeof error.message === 'string') {
      return error.message
    }
    if ('detail' in error && typeof error.detail === 'string') {
      return error.detail
    }
    if ('error' in error && typeof error.error === 'string') {
      return error.error
    }
    // Try to stringify the error object
    try {
      return JSON.stringify(error)
    } catch {
      return String(error)
    }
  }

  return String(error)
}

export { client }
