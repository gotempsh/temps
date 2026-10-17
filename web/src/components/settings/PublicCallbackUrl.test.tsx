// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'

import { getPublicIngressStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import { PublicCallbackUrl } from './PublicCallbackUrl'

const PATH = '/api/webhook/git/github/events'

function render(
  prepare: (queryClient: QueryClient) => void = () => {},
  props: { codeClassName?: string; prefix?: string } = {}
) {
  // `retryOnMount: false` keeps a failed query in its error state for the
  // render, which is what a mounted component sees once its retries are spent.
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retryOnMount: false } },
  })
  prepare(queryClient)
  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <PublicCallbackUrl path={PATH} label="Copy webhook URL" {...props} />
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

  describe('styling and keyboard access', () => {
    const known = (queryClient: QueryClient) => {
      queryClient.setQueryData(getPublicIngressStatusOptions().queryKey, {
        effective_url: 'https://hooks.example.com',
        source: 'explicit',
        configured: true,
      })
    }
    const regionClass = (html: string) =>
      /<div[^>]*class="([^"]*)"[^>]*>\s*<code/.exec(html)?.[1] ?? ''

    test('codeClassName replaces the default surface and padding', () => {
      const html = render(known, { codeClassName: 'bg-transparent p-0' })
      const classes = regionClass(html).split(' ')

      expect(classes).toContain('bg-transparent')
      expect(classes).toContain('p-0')
      expect(classes).not.toContain('bg-muted')
      expect(classes).not.toContain('px-2')
      expect(classes).not.toContain('py-1')
    })

    test('without overrides the URL sits on a muted, padded surface', () => {
      const classes = regionClass(render(known)).split(' ')

      expect(classes).toContain('bg-muted')
      expect(classes).toContain('px-2')
      expect(classes).toContain('overflow-auto')
    })

    test('a vertical-padding override replaces the default instead of adding to it', () => {
      const classes = regionClass(
        render(known, { codeClassName: 'py-1.5' })
      ).split(' ')

      expect(classes).toContain('py-1.5')
      expect(classes).not.toContain('py-1')
      expect(classes).toContain('px-2')
    })
  })
})
