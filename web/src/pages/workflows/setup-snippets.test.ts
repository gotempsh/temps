// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  defineSnippet,
  devCommand,
  setupStacks,
  startSnippet,
} from './setup-snippets'

describe('setup snippets', () => {
  test('every stack serves the executor endpoints with the workflow credential', () => {
    for (const { value } of setupStacks) {
      const { code } = defineSnippet(value)
      // Each one defines the same workflow, so the guide reads the same in every tab.
      expect(code).toContain('onboarding')
      expect(code).toContain('send-welcome')
      if (value === 'node' || value === 'nextjs') {
        expect(code).toContain("from '@temps-sdk/workflows'")
        expect(code).toContain('createWorkflowHandler')
      }
    }
    expect(defineSnippet('python').code).toContain('TEMPS_WORKFLOWS_TOKEN')
    expect(defineSnippet('go').code).toContain('TEMPS_WORKFLOWS_TOKEN')
    expect(defineSnippet('go').code).toContain(
      'github.com/gotempsh/temps/sdks/go/workflows'
    )
  })
  test('the Next.js route builds without TEMPS_API_URL', () => {
    // `next build` evaluates route modules; the handler must be created per request.
    const { code } = defineSnippet('nextjs')
    expect(code).toContain('.well-known/temps/workflows/[action]/route.ts')
    expect(code).toContain('handler ??=')
    expect(code).toContain('export const POST')
  })
  test('the dev command names the environment, or placeholders without one', () => {
    expect(devCommand({ projectId: 4, environmentId: 9 })).toBe(
      'temps workflows dev --project-id 4 --environment-id 9 --port 3000'
    )
    expect(devCommand({})).toContain('--project-id <project-id>')
  })
  test('starting a run deduplicates by a stable key', () => {
    expect(startSnippet).toContain('idempotencyKey')
  })
})

test('the Next.js route exports only its handlers, so next build accepts it', () => {
  const { code } = defineSnippet('nextjs')
  const exported = [...code.matchAll(/^export (?:const|function) (\w+)/gm)].map(
    (match) => match[1]
  )
  expect(exported).toEqual(['GET', 'POST'])
})
