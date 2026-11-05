// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  ForceJoinOption,
  WorkerJoinScript,
  WorkerRequirements,
} from './WorkerJoinScript'
import { workerJoinScript } from '@/lib/worker-join-script'

function render(node: ReactNode): string {
  return renderToStaticMarkup(<MemoryRouter>{node}</MemoryRouter>)
}

/** Markup as the user reads it: tags dropped, entities decoded. */
function text(html: string): string {
  return html
    .replace(/<[^>]+>/g, '')
    .replace(/&#x27;|&#39;/g, "'")
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&amp;/g, '&')
    .replace(/&quot;/g, '"')
}

describe('WorkerRequirements', () => {
  test('says up front that the worker must run Linux and when --private-address applies', () => {
    const shown = text(render(<WorkerRequirements />))
    expect(shown).toContain('The new machine must run Linux')
    expect(shown).toContain('A Mac or Windows machine cannot join this way')
    expect(shown).toContain('temps join --private-address <ip>')
    expect(shown).toContain('this server can reach at that address')
    expect(shown).toContain('cannot reach a machine behind a home router')
    expect(shown).toContain('network policy boot guard')
    expect(shown).toContain('The commands on this page are for Linux')
  })
})

describe('ForceJoinOption', () => {
  test('explains what happens without --force before anyone runs the join', () => {
    const shown = text(
      render(<ForceJoinOption checked={false} onChange={() => {}} />)
    )
    expect(shown).toContain('already a worker of another Temps cluster')
    expect(shown).toContain('stops on such a machine (or')
    expect(shown).toContain('cannot verify) and changes nothing')
    expect(shown).not.toContain('The old cluster loses this worker')
  })

  test('warns that the old cluster loses the worker once chosen', () => {
    const shown = text(render(<ForceJoinOption checked onChange={() => {}} />))
    expect(shown).toContain('The old cluster loses this worker')
  })
})

describe('WorkerJoinScript', () => {
  const script = workerJoinScript({
    url: 'https://cp.example.com',
    token: 'tk_enroll',
    version: 'v0.1.0-nightly.20261104.0123abcde',
    joinFlags: ' --ingress delegated',
  })

  test('shows the sudo commands and the pinned release', () => {
    const shown = text(render(<WorkerJoinScript {...script} />))
    expect(shown).toContain(
      'curl -fsSL https://temps.sh/install.sh | sudo -H bash -s -- v0.1.0-nightly.20261104.0123abcde) && \\'
    )
    expect(shown).toContain('Each step runs only if the one before it worked')
    expect(shown).toContain(
      'sudo -H /root/.temps/bin/temps join https://cp.example.com tk_enroll --ingress delegated'
    )
    expect(shown).toContain(
      'sudo -H /root/.temps/bin/temps agent service install'
    )
    expect(shown).toContain(
      'Installs v0.1.0-nightly.20261104.0123abcde, the version this server runs'
    )
    expect(shown).not.toContain('Run this as root')
  })

  test('shows no command while the server version is still being read', () => {
    const shown = text(render(<WorkerJoinScript {...script} pending />))
    expect(shown).toContain('Reading the version this server runs')
    expect(shown).not.toContain('temps join')
  })

  test('tells the user when the release could not be pinned', () => {
    const unpinned = workerJoinScript({
      url: 'https://cp.example.com',
      token: 'tk_enroll',
      version: undefined,
    })
    const html = render(<WorkerJoinScript {...unpinned} />)
    const shown = text(html)
    expect(shown).toContain('newest stable release, which may not match it')
    expect(shown).toContain('bash -s -- <version>')
    expect(html).toContain('href="/settings/version"')
  })
})
