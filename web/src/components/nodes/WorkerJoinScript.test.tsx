// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  ForceJoinOption,
  WorkerJoinScript,
  WorkerOsChoice,
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

describe('WorkerOsChoice', () => {
  test('offers Linux and Mac, and says what a Mac worker is for', () => {
    const shown = text(
      render(<WorkerOsChoice value="macos" onChange={() => {}} />)
    )
    expect(shown).toContain('What does the new machine run?')
    expect(shown).toContain('Linux')
    expect(shown).toContain(
      'Docker Desktop, OrbStack, colima or Rancher Desktop'
    )
    expect(shown).toContain('Runs as you, without sudo')
    expect(shown).toContain('development and personal use')
    expect(shown).toContain('offline while the Mac sleeps')
    expect(shown).not.toContain('must run Linux')
    expect(shown).toContain('temps join --private-address')
  })
})

describe('WorkerJoinScript for a Mac', () => {
  test('tells the user to paste it as themselves with the Docker app running', () => {
    const shown = text(
      render(
        <WorkerJoinScript
          command="x"
          install={{ kind: 'release', tag: 'v0.1.0' }}
          os="macos"
        />
      )
    )
    expect(shown).toContain('Paste this in Terminal on the Mac, as yourself')
    expect(shown).toContain('Docker app running')
    expect(shown).not.toContain('leave out sudo -H')
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
    install: { kind: 'release', tag: 'v0.1.0-nightly.20261104.0123abcde' },
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
      install: { kind: 'latest' },
    })
    const html = render(<WorkerJoinScript {...unpinned} />)
    const shown = text(html)
    expect(shown).toContain('newest stable release, which may not match it')
    expect(shown).toContain('bash -s -- <version>')
    expect(html).toContain('href="/settings/version"')
  })

  test('a build between releases installs the newest nightly and says why', () => {
    const between = workerJoinScript({
      url: 'https://cp.example.com',
      token: 'tk_enroll',
      install: { kind: 'nightly', after: 'v0.1.0-beta.61' },
    })
    const shown = text(render(<WorkerJoinScript {...between} />))
    expect(shown).toContain(
      'curl -fsSL https://temps.sh/install.sh | sudo -H bash -s -- --channel nightly) && \\'
    )
    expect(shown).not.toContain('bash -s -- v0.1.0-beta.61')
    expect(shown).toContain(
      'This server runs a build made after v0.1.0-beta.61, which no release contains'
    )
    expect(shown).toContain('installs the newest nightly build')
  })
})
