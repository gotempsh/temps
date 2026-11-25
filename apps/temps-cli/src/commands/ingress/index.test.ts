// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { describeNetwork, describePrivateStatus, networkName, privateIntent } from './index.js'

describe('privateIntent', () => {
  test('no flags shows the status', () => {
    expect(privateIntent({})).toEqual({ kind: 'status' })
  })

  test('--listen tailscale stores a consent without Tailscale Serve', () => {
    expect(privateIntent({ listen: 'tailscale' })).toEqual({
      kind: 'serve',
      request: { listen: 'tailscale', admit: [], tailscale_serve: false },
    })
  })

  test('--admit names who may connect on any private network', () => {
    expect(privateIntent({ listen: '10.8.0.1', admit: [' 10.8.0.0/24 ', '192.168.50.0/24'] })).toEqual({
      kind: 'serve',
      request: { listen: '10.8.0.1', admit: ['10.8.0.0/24', '192.168.50.0/24'], tailscale_serve: false },
    })
    expect(privateIntent({ admit: ['10.8.0.0/24'] }).kind).toBe('invalid')
    expect(privateIntent({ off: true, admit: ['10.8.0.0/24'] }).kind).toBe('invalid')
  })

  test('--tailscale-serve is carried with --listen', () => {
    expect(privateIntent({ listen: ' 100.64.0.7 ', tailscaleServe: true })).toEqual({
      kind: 'serve',
      request: { listen: '100.64.0.7', admit: [], tailscale_serve: true },
    })
  })

  test('--off withdraws the consent', () => {
    expect(privateIntent({ off: true })).toEqual({ kind: 'off' })
  })

  test('--off with --listen is refused', () => {
    expect(privateIntent({ off: true, listen: 'tailscale' }).kind).toBe('invalid')
  })

  test('--tailscale-serve alone is refused', () => {
    expect(privateIntent({ tailscaleServe: true }).kind).toBe('invalid')
  })

  test('an empty --listen is refused', () => {
    expect(privateIntent({ listen: '  ' }).kind).toBe('invalid')
  })
})

describe('describePrivateStatus', () => {
  const consent = { listen: 'tailscale', tailscale_serve: false, updated_at: '2026-11-24T10:00:00Z' }

  test('without consent nothing is served', () => {
    expect(describePrivateStatus({ consent: null })).toBe('This machine does not serve private addresses.')
  })

  test('names the address it serves on and who it admits', () => {
    expect(describePrivateStatus({ consent, serving: '100.64.0.7:443' })).toBe(
      'Serving private addresses on 100.64.0.7:443.',
    )
    expect(describePrivateStatus({ consent, serving: '10.8.0.1', admit: ['10.8.0.0/24'] })).toBe(
      'Serving private addresses on 10.8.0.1, to 10.8.0.0/24 only.',
    )
  })

  test('says why it does not serve', () => {
    expect(describePrivateStatus({ consent, error: 'This machine has no Tailscale address' })).toBe(
      'Not serving: This machine has no Tailscale address',
    )
  })

  test('before the first report it is waiting', () => {
    expect(describePrivateStatus({ consent })).toBe('Waiting for this machine to apply it.')
  })
})

describe('networks', () => {
  test('each says what it admits and how to choose it, or why it cannot be used', () => {
    const wg = { interface: 'wg0', address: '10.8.0.1', tailscale: false, admit: ['10.8.0.0/24'], problem: null }
    expect(networkName(wg)).toBe('wg0 (10.8.0.1)')
    expect(describeNetwork(wg)).toBe('admits 10.8.0.0/24 · --listen 10.8.0.1')
    const ts = { ...wg, interface: 'tailscale0', address: '100.101.102.103', tailscale: true, admit: ['100.64.0.0/10'] }
    expect(networkName(ts)).toBe('Tailscale (100.101.102.103)')
    expect(describeNetwork(ts)).toBe('admits 100.64.0.0/10 · --listen tailscale')
    expect(describeNetwork({ ...wg, admit: [], problem: 'it has no subnet' })).toBe(
      'Cannot be used: it has no subnet',
    )
  })
})
