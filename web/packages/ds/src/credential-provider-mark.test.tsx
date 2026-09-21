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
    expect(html).not.toContain('bg-white')
    expect(html).not.toContain('image/png')
    const svg = Buffer.from(asset.src.split(',')[1], 'base64').toString()
    expect(svg).toContain('<svg')
    expect(svg).not.toMatch(/<image\b/)
    // Rectangles inside clip paths define clipping, not opaque backgrounds.
    expect(svg.replace(/<defs>[\s\S]*?<\/defs>/g, '')).not.toMatch(/<rect\b/)
    if (asset.darkSrc) {
      expect(html).toContain('dark:hidden')
      expect(html).toContain('dark:block')
      expect(
        Buffer.from(asset.darkSrc.split(',')[1], 'base64').toString()
      ).toContain('<svg')
    }
    if (provider === 'gitlab') expect(html).not.toContain('invert')
    if (provider === 'anthropic') expect(html).toContain('dark:invert')
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

// Prevent substituting the wide company wordmark in compact credential rows.
test('Anthropic uses the compact company symbol in the standard square slot', () => {
  const svg = Buffer.from(
    credentialProviderAssets.anthropic.src.split(',')[1],
    'base64'
  ).toString()
  expect(svg).toContain('viewBox="0 0 24 24"')
  expect(svg).toContain('M17.3041 3.541h-3.6718')
  const html = renderToStaticMarkup(
    <CredentialProviderMark provider="anthropic" />
  )
  expect(html).toContain('width="24"')
  expect(html).toContain('size-6')
  expect(html).not.toContain('w-20')
  expect(html).toContain('dark:invert')
})
