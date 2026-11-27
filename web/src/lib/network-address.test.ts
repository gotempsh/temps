// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  consoleReach,
  isLoopbackHost,
  isTailscaleHost,
  parseIpv4,
} from './network-address'

describe('consoleReach', () => {
  test('loopback addresses', () => {
    for (const origin of [
      'http://localhost:3014',
      'http://app.localhost',
      'http://127.0.0.1:8080',
      'http://127.8.9.10',
      'http://[::1]:3000',
    ]) {
      expect(consoleReach(origin)).toBe('loopback')
    }
  })

  test('private networks, Tailscale included', () => {
    for (const origin of [
      'http://10.1.2.3',
      'http://172.16.0.1',
      'http://172.31.255.255',
      'http://192.168.1.10:8080',
      'http://169.254.1.1',
      'http://100.64.0.7',
      'http://100.127.0.1',
      'https://server.tailnet-1234.ts.net',
      'http://[fd7a:115c:a1e0::1]',
      'http://[fd12:3456::1]',
      'http://nas:3000',
      'http://temps.local',
      'http://temps.home.arpa',
    ]) {
      expect(consoleReach(origin)).toBe('private')
    }
  })

  test('public addresses and what only looks private', () => {
    for (const origin of [
      'https://temps.example.com',
      'http://172.32.0.1',
      'http://100.128.0.1',
      'http://8.8.8.8',
      'http://192.169.0.1',
      'https://local.example.com',
      'not a url',
    ]) {
      expect(consoleReach(origin)).toBe('public')
    }
  })
})

test('IPv4 literals only', () => {
  expect(parseIpv4('10.0.0.1')).toEqual([10, 0, 0, 1])
  expect(parseIpv4('256.0.0.1')).toBeNull()
  expect(parseIpv4('10.0.0')).toBeNull()
  expect(parseIpv4('10.0.0.1.example.com')).toBeNull()
})

test('host classes', () => {
  expect(isLoopbackHost('[::1]')).toBe(true)
  expect(isLoopbackHost('localhost.example.com')).toBe(false)
  expect(isTailscaleHost('100.100.100.100')).toBe(true)
  expect(isTailscaleHost('100.63.0.1')).toBe(false)
  expect(isTailscaleHost('ts.net.example.com')).toBe(false)
})
