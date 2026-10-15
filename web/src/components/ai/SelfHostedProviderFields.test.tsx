// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { SelfHostedProviderFields } from './SelfHostedProviderFields'
import { DEFAULT_SELF_HOSTED_CAPABILITIES } from './self-hosted-provider'

function render(open: boolean) {
  const noop = () => {}
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>
        <SelfHostedProviderFields
          open={open}
          baseUrl="http://10.0.0.5:8000/v1"
          apiKey=""
          trustPrivateAddress={false}
          onTrustPrivateAddressChange={noop}
          canTrustPrivateAddress
          discovery={null}
          onDiscovered={noop}
          defaultModel=""
          onDefaultModelChange={noop}
          capabilities={DEFAULT_SELF_HOSTED_CAPABILITIES}
          onCapabilitiesChange={noop}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('stays mounted but renders nothing while another provider is selected', () => {
  expect(render(false)).toBe('')
})

test('shows trust, model discovery and capabilities for a self-hosted provider', () => {
  const html = render(true)
  expect(html).toContain('Trust this private address for this key only')
  expect(html).toContain('Load models')
  expect(html).toContain('Plain http:// is only accepted')
  expect(html).toContain('Responses API')
})
