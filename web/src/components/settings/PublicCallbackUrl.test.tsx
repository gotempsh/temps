// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'

import { getPublicIngressStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import { PublicCallbackUrl } from './PublicCallbackUrl'

const PATH = '/api/webhook/git/github/events'

function render(prepare: (queryClient: QueryClient) => void = () => {}) {
  // `retryOnMount: false` keeps a failed query in its error state for the
  // render, which is what a mounted component sees once its retries are spent.
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retryOnMount: false } },
  })
  prepare(queryClient)
  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <PublicCallbackUrl path={PATH} label="Copy webhook URL" />
    </QueryClientProvider>
  )
}

describe('PublicCallbackUrl', () => {
  test('shows the URL built from the public ingress URL once it is known', () => {
    const html = render((queryClient) => {
      queryClient.setQueryData(getPublicIngressStatusOptions().queryKey, {
        effective_url: 'https://hooks.example.com',
        source: 'explicit',
        configured: true,
      })
    })

    expect(html).toContain(
      'https://hooks.example.com/api/webhook/git/github/events'
    )
    expect(html).toContain('aria-label="Copy webhook URL"')
  })

  test('offers nothing to copy while the ingress URL is loading', () => {
    const html = render()

    // No URL at all: not the ingress URL, and not a guessed console origin.
    expect(html).not.toContain(PATH)
    expect(html).not.toContain('http')
    expect(html).not.toContain('aria-label="Copy webhook URL"')
    expect(html).not.toContain('Retry')
  })

  test('explains a failed read and offers a retry instead of a guessed URL', () => {
    const html = render((queryClient) => {
      queryClient
        .getQueryCache()
        .build(queryClient, {
          queryKey: getPublicIngressStatusOptions().queryKey,
        })
        .setState({
          status: 'error',
          fetchStatus: 'idle',
          error: new Error('request failed'),
          data: undefined,
        })
    })

    expect(html).toContain('role="alert"')
    expect(html).toContain('load the public callback URL')
    expect(html).toContain('Retry')
    expect(html).not.toContain(PATH)
    expect(html).not.toContain('aria-label="Copy webhook URL"')
  })
})
