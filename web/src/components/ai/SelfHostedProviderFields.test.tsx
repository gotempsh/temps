// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { SelfHostedProviderFields } from './SelfHostedProviderFields'
import {
  DEFAULT_SELF_HOSTED_CAPABILITIES,
  type SelfHostedCapabilitiesInput,
} from './self-hosted-provider'

function render(
  open: boolean,
  {
    baseUrl = 'http://10.0.0.5:8000/v1',
    trustPrivateAddress = false,
    capabilities = DEFAULT_SELF_HOSTED_CAPABILITIES,
  }: {
    baseUrl?: string
    trustPrivateAddress?: boolean
    capabilities?: SelfHostedCapabilitiesInput
  } = {}
) {
  const noop = () => {}
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>
        <SelfHostedProviderFields
          open={open}
          baseUrl={baseUrl}
          apiKey=""
          trustPrivateAddress={trustPrivateAddress}
          onTrustPrivateAddressChange={noop}
          canTrustPrivateAddress
          discovery={null}
          onDiscovered={noop}
          defaultModel=""
          onDefaultModelChange={noop}
          capabilities={capabilities}
          onCapabilitiesChange={noop}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('stays mounted but renders nothing while another provider is selected', () => {
  expect(render(false)).toBe('')
})

test('a private endpoint offers trust, model discovery and the advanced capabilities', () => {
  const html = render(true)
  expect(html).toContain('Trust this private address for this key only')
  expect(html).toContain('Load models')
  expect(html).toContain('Plain http:// is only accepted')
  expect(html).toContain('Advanced: what your server supports')
})

test('a public endpoint is not offered private address trust', () => {
  const html = render(true, { baseUrl: 'https://api.example.com/v1' })
  expect(html).not.toContain('Trust this private address')
  expect(html).toContain('Load models')
})

test('a trust choice made for an earlier private URL can still be taken back', () => {
  const html = render(true, {
    baseUrl: 'https://api.example.com/v1',
    trustPrivateAddress: true,
  })
  expect(html).toContain('Trust this private address for this key only')
})

test('capabilities are all declared and folded away by default', () => {
  const html = render(true)
  expect(html).not.toContain('Responses API')
})

test('a narrowed declaration opens the advanced section so it is visible', () => {
  const html = render(true, {
    capabilities: { ...DEFAULT_SELF_HOSTED_CAPABILITIES, responses_api: false },
  })
  expect(html).toContain('Responses API')
})
