// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import path from 'node:path'

/**
 * The AI gateway is mounted under the API prefix, so the OpenAI-compatible
 * base URL is `<instance>/api/ai/v1`. A bare `<instance>/ai/v1` is not served
 * by the gateway at all: it falls through to the console SPA and answers
 * `200 text/html`, which every SDK quick-start then fails to parse.
 *
 * Source scan over the documentation and the in-console snippets that tell
 * users which URL to configure.
 */

const REPO_ROOT = path.resolve(import.meta.dir, '../../..')

const FILES = [
  'docs/features/ai-gateway/page.mdx',
  'docs/features/ai-tracing/page.mdx',
  'docs/howto/admin-listener/page.mdx',
  'web/src/pages/AiGatewaySetupPage.tsx',
  'web/src/pages/AiGateway.tsx',
]

/** Every `ai/v1` occurrence that is not directly preceded by `/api/`. */
const BARE_GATEWAY_PATH = /(?<!\/api)\/ai\/v1\b/g

test.each(FILES)('%s only references the gateway under /api/ai/v1', (file) => {
  const source = readFileSync(path.join(REPO_ROOT, file), 'utf8')
  const offenders = source
    .split('\n')
    .map((text, index) => ({ text, line: index + 1 }))
    .filter(({ text }) => new RegExp(BARE_GATEWAY_PATH).test(text))
    .map(({ text, line }) => `${file}:${line}: ${text.trim()}`)
  expect(offenders).toEqual([])
})

test('the quick-start documents the /api/ai/v1 base URL', () => {
  const source = readFileSync(
    path.join(REPO_ROOT, 'docs/features/ai-gateway/page.mdx'),
    'utf8'
  )
  expect(source).toContain('https://your-temps-instance.com/api/ai/v1')
})

test.each([
  'web/src/pages/AiGatewaySetupPage.tsx',
  'web/src/pages/AiGateway.tsx',
])('%s derives its endpoint from the /api prefix', (file) => {
  const source = readFileSync(path.join(REPO_ROOT, file), 'utf8')
  expect(source).toContain('`${externalUrl}/api/ai/v1`')
})
