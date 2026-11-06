// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { UnavailableNote } from './ServiceResourcesPanel'

describe('UnavailableNote', () => {
  test('says why the container state could not be read', () => {
    const markup = renderToStaticMarkup(
      <UnavailableNote
        runtimeReason="the agent on node worker-3 (id 3) does not report container runtime state; update the agent on node worker-3 to this release"
        statsReason="stats unavailable: the agent on node worker-3 (id 3) does not report resource usage; update the agent on node worker-3 to this release"
      />
    )
    expect(markup).toContain('Container state unavailable')
    expect(markup).toContain('does not report container runtime state')
    expect(markup).toContain('stats unavailable')
  })

  test('only usage missing is labelled as usage', () => {
    const markup = renderToStaticMarkup(
      <UnavailableNote statsReason="stats unavailable: agent unreachable" />
    )
    expect(markup).toContain('Resource usage unavailable')
  })

  test('renders nothing when everything was read', () => {
    expect(renderToStaticMarkup(<UnavailableNote />)).toBe('')
  })
})
