// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { domainProblem, type AccessChoice } from './access'
import {
  saveReach,
  type ReachSaveProgress,
  type ReachSaveSteps,
} from './reach-save'

const PRIVATE_DOMAIN: AccessChoice = {
  audience: 'private',
  way: 'tailnet_domain',
  naming: 'wildcard',
  domain: 'apps.example.com',
  token: 'x'.repeat(40),
}
const FACTS = {
  privateNetwork: {
    label: 'Tailscale',
    address: '100.101.102.103',
    listen: 'tailscale',
    admit: ['100.64.0.0/10'],
  },
}

/** Steps that record each call and fail where told to. */
function recorder(failing: Partial<Record<keyof ReachSaveSteps, number>> = {}) {
  const calls: string[] = []
  const left = { ...failing }
  const step =
    (name: keyof ReachSaveSteps) =>
    async (arg: unknown): Promise<unknown> => {
      calls.push(
        `${name}:${typeof arg === 'string' ? arg : JSON.stringify(arg)}`
      )
      if ((left[name] ?? 0) > 0) {
        left[name] = (left[name] ?? 0) - 1
        throw new Error(`${name} failed`)
      }
      return undefined
    }
  const steps: ReachSaveSteps = {
    setPreviewDomain: step('setPreviewDomain'),
    createDnsProvider: step('createDnsProvider'),
    servePrivately: step('servePrivately'),
    recordReach: step('recordReach'),
  }
  const count = (name: keyof ReachSaveSteps) =>
    calls.filter((c) => c.startsWith(`${name}:`)).length
  return { steps, calls, count }
}

describe('saveReach', () => {
  test('a private domain creates the provider, consents, then records the choice', async () => {
    const { steps, calls } = recorder()
    const reach = await saveReach(PRIVATE_DOMAIN, FACTS, steps, {})
    expect(reach.domain).toBe('apps.example.com')
    expect(calls.map((c) => c.split(':')[0])).toEqual([
      'createDnsProvider',
      'servePrivately',
      'recordReach',
    ])
    expect(calls[1]).toBe('servePrivately:tailscale')
  })

  test('saving again after a later write failed never stores the token twice', async () => {
    const { steps, count } = recorder({ recordReach: 1, servePrivately: 1 })
    const progress: ReachSaveProgress = {}
    await expect(
      saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    ).rejects.toThrow('servePrivately failed')
    await expect(
      saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    ).rejects.toThrow('recordReach failed')
    await saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    expect(count('createDnsProvider')).toBe(1)
    expect(count('servePrivately')).toBe(3)
    expect(count('recordReach')).toBe(2)
  })

  test('a provider that failed to be created is created on the next try', async () => {
    const { steps, count } = recorder({ createDnsProvider: 1 })
    const progress: ReachSaveProgress = {}
    await expect(
      saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    ).rejects.toThrow()
    await saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    expect(count('createDnsProvider')).toBe(2)
    expect(count('servePrivately')).toBe(1)
  })

  test('another zone or token is another provider', async () => {
    const { steps, count } = recorder()
    const progress: ReachSaveProgress = {}
    await saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    await saveReach(
      { ...PRIVATE_DOMAIN, domain: 'apps.example.org' },
      FACTS,
      steps,
      progress
    )
    await saveReach(
      { ...PRIVATE_DOMAIN, domain: 'apps.example.org', token: 'y'.repeat(40) },
      FACTS,
      steps,
      progress
    )
    expect(count('createDnsProvider')).toBe(3)
  })

  test('open ports set the preview domain and write no provider', async () => {
    const { steps, calls } = recorder()
    await saveReach(
      {
        audience: 'public',
        way: 'direct',
        naming: 'wildcard',
        domain: 'apps.example.com',
        token: '',
      },
      FACTS,
      steps,
      {}
    )
    expect(calls).toEqual([
      'setPreviewDomain:apps.example.com',
      expect.stringMatching(/^recordReach:/),
    ])
  })
})

describe('domainProblem matches the server', () => {
  test('a label over 63 characters is refused before anything is written', () => {
    expect(domainProblem(`${'a'.repeat(63)}.example.com`)).toBeNull()
    expect(domainProblem(`${'a'.repeat(64)}.example.com`)?.reason).toContain(
      '63 characters'
    )
  })

  test('a name over 253 characters is refused', () => {
    const label = 'a'.repeat(60)
    const name = `${[label, label, label, label].join('.')}.example.com`
    expect(name.length).toBeGreaterThan(253)
    expect(domainProblem(name)?.reason).toContain('253 characters')
  })
})
