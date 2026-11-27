// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { domainProblem, type AccessChoice } from './access'
import {
  CONSENT_CHANGED,
  hasDnsProvider,
  NO_PRIVATE_NETWORK,
  sessionAttemptRef,
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
const NETWORK = {
  label: 'Tailscale',
  address: '100.101.102.103',
  listen: 'tailscale',
  admit: ['100.64.0.0/10'],
}
/** A plan made when there was no consent. */
const FACTS = { privateNetwork: NETWORK }
/** A plan made from the consent given at `at`. */
const keeping = (at: string) => ({
  privateNetwork: { ...NETWORK, consented: true, consentAt: at },
})

/**
 * Steps that record each call and fail where told to, against a server
 * whose consent was given at `consentAt` (none when `null`). It gives one
 * only where there is none, as `create_only` asks.
 */
function recorder(
  failing: Partial<Record<keyof ReachSaveSteps, number>> = {},
  consentAt: string | null = null,
  /** Provider creations that land on the server but whose answer is lost. */
  lostResponses = 0
) {
  const calls: string[] = []
  const left = { ...failing }
  const server = {
    consentAt,
    given: 0,
    providers: [] as string[],
    lost: lostResponses,
  }
  const refs = new Map<string, string>()
  const fail = (name: keyof ReachSaveSteps) => {
    if ((left[name] ?? 0) > 0) {
      left[name] = (left[name] ?? 0) - 1
      throw new Error(`${name} failed`)
    }
  }
  const step =
    (name: keyof ReachSaveSteps) =>
    async (arg: unknown): Promise<unknown> => {
      calls.push(
        `${name}:${typeof arg === 'string' ? arg : JSON.stringify(arg)}`
      )
      fail(name)
      return undefined
    }
  const steps: ReachSaveSteps = {
    setPreviewDomain: step('setPreviewDomain'),
    createDnsProvider: async (body) => {
      calls.push(`createDnsProvider:${JSON.stringify(body)}`)
      fail('createDnsProvider')
      server.providers.push(body.description)
      if (server.lost > 0) {
        server.lost -= 1
        throw new Error('the response was lost')
      }
    },
    attemptRef: async (key) => {
      if (!refs.has(key)) refs.set(key, `ref${refs.size + 1}`)
      return refs.get(key) as string
    },
    findDnsProvider: async (description) =>
      server.providers.includes(description),
    servePrivately: async (listen) => {
      calls.push(`servePrivately:${listen}`)
      fail('servePrivately')
      if (server.consentAt)
        throw new Error('A private ingress consent was given')
      server.given += 1
      server.consentAt = `given-${server.given}`
      return { updated_at: server.consentAt }
    },
    recordReach: step('recordReach'),
    currentPrivateConsent: async () =>
      server.consentAt === null ? null : { updated_at: server.consentAt },
  }
  const count = (name: keyof ReachSaveSteps) =>
    calls.filter((c) => c.startsWith(`${name}:`)).length
  return { steps, calls, count, server }
}

const names = (calls: string[]) => calls.map((c) => c.split(':')[0])

describe('saveReach', () => {
  test('a private domain consents, creates the provider, then records the choice', async () => {
    const { steps, calls } = recorder()
    const reach = await saveReach(PRIVATE_DOMAIN, FACTS, steps, {})
    expect(reach.domain).toBe('apps.example.com')
    expect(names(calls)).toEqual([
      'servePrivately',
      'createDnsProvider',
      'recordReach',
    ])
    expect(calls[0]).toBe('servePrivately:tailscale')
  })

  test('a consent given before, with its own ranges, is kept as it is', async () => {
    const { steps, calls } = recorder({}, 't1')
    await saveReach(PRIVATE_DOMAIN, keeping('t1'), steps, {})
    expect(names(calls)).toEqual(['createDnsProvider', 'recordReach'])
  })

  test('a consent withdrawn, changed or given after the plan was made: nothing is written', async () => {
    for (const [plan, now] of [
      // Withdrawn: giving it again would admit the interface's whole subnet.
      [keeping('t1'), null],
      // Changed (other ranges on the same listener, or another listener).
      [keeping('t1'), 't2'],
      // Given meanwhile, with ranges of its own.
      [FACTS, 't2'],
    ] as const) {
      const { steps, calls } = recorder({}, now)
      await expect(saveReach(PRIVATE_DOMAIN, plan, steps, {})).rejects.toThrow(
        CONSENT_CHANGED
      )
      // No provider to store twice after the reload it asks for.
      expect(calls).toEqual([])
    }
  })

  test('a consent given between the check and the write is kept, and nothing else is written', async () => {
    const { steps, calls, server } = recorder()
    const serve = steps.servePrivately
    steps.servePrivately = async (listen) => {
      server.consentAt = 'given-elsewhere'
      return serve(listen)
    }
    await expect(saveReach(PRIVATE_DOMAIN, FACTS, steps, {})).rejects.toThrow(
      'consent was given'
    )
    expect(names(calls)).toEqual(['servePrivately'])
    expect(server.consentAt).toBe('given-elsewhere')
  })

  test('saving again after a later write failed never consents or stores the token twice', async () => {
    const { steps, count } = recorder({ createDnsProvider: 1, recordReach: 1 })
    const progress: ReachSaveProgress = {}
    await expect(
      saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    ).rejects.toThrow('createDnsProvider failed')
    // The consent this save gave is the one the next try expects.
    await expect(
      saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    ).rejects.toThrow('recordReach failed')
    await saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    expect(count('servePrivately')).toBe(1)
    expect(count('createDnsProvider')).toBe(2)
    expect(count('recordReach')).toBe(2)
  })

  test('a provider deleted or disabled before a retry is made again, never taken as done', async () => {
    const { steps, count, server } = recorder({ recordReach: 1 })
    const progress: ReachSaveProgress = {}
    await expect(
      saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    ).rejects.toThrow('recordReach failed')
    expect(server.providers).toHaveLength(1)
    // Another administrator deletes it (or disables it) meanwhile.
    server.providers.splice(0)
    await saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    expect(count('createDnsProvider')).toBe(2)
    expect(server.providers).toHaveLength(1)
    expect(count('recordReach')).toBe(2)
  })

  test('a provider stored whose answer was lost, or before a reload, is found, never stored twice', async () => {
    const { steps, count, server } = recorder({}, null, 1)
    await expect(saveReach(PRIVATE_DOMAIN, FACTS, steps, {})).rejects.toThrow(
      'the response was lost'
    )
    // A reload: nothing in memory, the consent read again with the facts.
    await saveReach(
      PRIVATE_DOMAIN,
      keeping(server.consentAt as string),
      steps,
      {}
    )
    expect(count('createDnsProvider')).toBe(1)
    expect(server.providers).toHaveLength(1)
    expect(server.providers[0]).toContain('(ref ref1)')
    expect(count('recordReach')).toBe(1)
  })

  test('a private way in with its network gone records nothing', async () => {
    const { steps, calls } = recorder()
    await expect(
      saveReach(PRIVATE_DOMAIN, { privateNetwork: null }, steps, {})
    ).rejects.toThrow(NO_PRIVATE_NETWORK)
    expect(calls).toEqual([])
  })

  test('a consent that failed to be given is given on the next try', async () => {
    const { steps, count } = recorder({ servePrivately: 1 })
    const progress: ReachSaveProgress = {}
    await expect(
      saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    ).rejects.toThrow('servePrivately failed')
    await saveReach(PRIVATE_DOMAIN, FACTS, steps, progress)
    expect(count('servePrivately')).toBe(2)
    expect(count('createDnsProvider')).toBe(1)
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
    expect(count('servePrivately')).toBe(1)
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

describe('sessionAttemptRef', () => {
  test('one reference per zone and token, kept by digest, never the token', async () => {
    const kept = new Map<string, string>()
    const storage = {
      getItem: (k: string) => kept.get(k) ?? null,
      setItem: (k: string, v: string) => void kept.set(k, v),
    }
    const a = await sessionAttemptRef(
      'Cloudflare · example.com\ntok_secret',
      storage
    )
    expect(
      await sessionAttemptRef('Cloudflare · example.com\ntok_secret', storage)
    ).toBe(a)
    expect(
      await sessionAttemptRef('Cloudflare · example.com\ntok_other', storage)
    ).not.toBe(a)
    expect([...kept.keys(), ...kept.values()].join(' ')).not.toContain('tok_')
  })

  test('works where crypto.subtle is missing, as on a console opened over plain HTTP', async () => {
    const subtle = Object.getOwnPropertyDescriptor(globalThis.crypto, 'subtle')
    Object.defineProperty(globalThis.crypto, 'subtle', {
      value: undefined,
      configurable: true,
    })
    try {
      const kept = new Map<string, string>()
      const storage = {
        getItem: (k: string) => kept.get(k) ?? null,
        setItem: (k: string, v: string) => void kept.set(k, v),
      }
      const ref = await sessionAttemptRef('zone\ntok', storage)
      expect(ref).toMatch(/^[0-9a-f]{12}$/)
      expect(await sessionAttemptRef('zone\ntok', storage)).toBe(ref)
    } finally {
      // Inherited, not its own, in Bun: removing the shadow brings it back.
      if (subtle) Object.defineProperty(globalThis.crypto, 'subtle', subtle)
      else delete (globalThis.crypto as { subtle?: unknown }).subtle
    }
    expect(globalThis.crypto.subtle).toBeDefined()
  })
})

describe('a DNS provider this save made already', () => {
  test('counts only while active: a disabled one is made again', () => {
    const description = 'Welcome: example.com (ref 0123456789ab)'
    expect(
      hasDnsProvider([{ is_active: true, description }], description)
    ).toBe(true)
    expect(
      hasDnsProvider([{ is_active: false, description }], description)
    ).toBe(false)
    expect(
      hasDnsProvider(
        [{ is_active: true, description: 'Welcome: example.com' }],
        description
      )
    ).toBe(false)
    expect(
      hasDnsProvider([{ is_active: true, description: null }], description)
    ).toBe(false)
  })
})
