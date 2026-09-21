// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { CredentialProviderMark } from './credential-provider-mark'
import { credentialProviderAssets } from './credential-provider-assets'

test('canonical providers have accessible locally bundled original marks', () => {
  for (const [provider, asset] of Object.entries(credentialProviderAssets)) {
    const html = renderToStaticMarkup(
      <CredentialProviderMark provider={provider} />
    )
    expect(html).toContain(`alt="${asset.name}"`)
    expect(html).toContain('src="data:image/')
    expect(html).toContain('object-contain')
    expect(html).toContain('bg-white')
    expect(html).not.toContain('https://')
  }
})

test('names, ambiguous IDs, and prototype keys never guess a company', () => {
  for (const provider of [
    undefined,
    null,
    '',
    'OPENAI_API_KEY',
    'github-pat',
    'GitHub',
    'custom',
    'constructor',
    '__proto__',
  ]) {
    const html = renderToStaticMarkup(
      <CredentialProviderMark provider={provider} />
    )
    expect(html).toContain('Custom or unknown provider')
    expect(html).not.toContain('<img')
  }
})
