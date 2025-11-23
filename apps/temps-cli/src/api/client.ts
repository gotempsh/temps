import { config, credentials } from '../config/store.js'
import { ApiError, AuthenticationError } from '../utils/errors.js'

export interface RequestOptions {
  params?: Record<string, string | number | boolean | undefined>
  body?: unknown
  headers?: Record<string, string>
}

export interface ApiResponse<T = unknown> {
  data: T
  status: number
}

class HttpClient {
  private baseUrl: string

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl.replace(/\/$/, '')
  }

  private async getHeaders(): Promise<Headers> {
    const headers = new Headers({
      'Content-Type': 'application/json',
    })

    const apiKey = await credentials.getApiKey()
    if (apiKey) {
      headers.set('X-Api-Key', apiKey)
    }

    return headers
  }

  private buildUrl(path: string, params?: Record<string, string | number | boolean | undefined>): string {
    let url = `${this.baseUrl}${path}`

    if (params) {
      const searchParams = new URLSearchParams()
      for (const [key, value] of Object.entries(params)) {
        if (value !== undefined) {
          searchParams.set(key, String(value))
        }
      }
      const queryString = searchParams.toString()
      if (queryString) {
        url += `?${queryString}`
      }
    }

    return url
  }

  private async request<T>(method: string, path: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    const url = this.buildUrl(path, options?.params)
    const headers = await this.getHeaders()

    if (options?.headers) {
      for (const [key, value] of Object.entries(options.headers)) {
        headers.set(key, value)
      }
    }

    const fetchOptions: RequestInit = {
      method,
      headers,
    }

    if (options?.body && method !== 'GET') {
      fetchOptions.body = JSON.stringify(options.body)
    }

    try {
      const response = await fetch(url, fetchOptions)

      if (response.status === 401) {
        await credentials.clear()
        throw new AuthenticationError('API key invalid or expired. Please run: temps login')
      }

      if (response.status === 204) {
        return { data: undefined as T, status: response.status }
      }

      const contentType = response.headers.get('content-type')
      let data: T

      if (contentType?.includes('application/json')) {
        data = (await response.json()) as T
      } else {
        data = (await response.text()) as T
      }

      if (!response.ok) {
        const errorMessage =
          typeof data === 'object' && data !== null && 'detail' in data
            ? String((data as { detail: string }).detail)
            : typeof data === 'object' && data !== null && 'message' in data
              ? String((data as { message: string }).message)
              : `Request failed with status ${response.status}`

        throw new ApiError(errorMessage, response.status, data)
      }

      return { data, status: response.status }
    } catch (error) {
      if (error instanceof ApiError || error instanceof AuthenticationError) {
        throw error
      }

      if (error instanceof TypeError && error.message.includes('fetch')) {
        throw new ApiError(`Cannot connect to API at ${this.baseUrl}. Is the server running?`)
      }

      throw new ApiError(error instanceof Error ? error.message : 'Unknown error')
    }
  }

  async get<T>(path: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request<T>('GET', path, options)
  }

  async post<T>(path: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request<T>('POST', path, options)
  }

  async put<T>(path: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request<T>('PUT', path, options)
  }

  async patch<T>(path: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request<T>('PATCH', path, options)
  }

  async delete<T>(path: string, options?: RequestOptions): Promise<ApiResponse<T>> {
    return this.request<T>('DELETE', path, options)
  }
}

let clientInstance: HttpClient | null = null

export function getClient(): HttpClient {
  if (!clientInstance) {
    clientInstance = new HttpClient(config.get('apiUrl'))
  }
  return clientInstance
}

export function resetClient(): void {
  clientInstance = null
}

export type ApiClient = HttpClient
