// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  describePathStyleReason,
  previewPathStyle,
  previewS3Endpoint,
} from './s3-endpoint'

describe('previewS3Endpoint', () => {
  test('a bare IP and port is plain HTTP', () => {
    expect(previewS3Endpoint('10.0.0.5:9000')).toEqual({
      url: 'http://10.0.0.5:9000',
      scheme: 'http',
      explicitScheme: false,
      hostIsIp: true,
    })
  })

  test('an explicit https scheme is kept and a trailing slash dropped', () => {
    expect(previewS3Endpoint('https://s3.example.com/')).toMatchObject({
      url: 'https://s3.example.com',
      scheme: 'https',
      explicitScheme: true,
      hostIsIp: false,
    })
  })

  test('a host whose name starts with http still gets a scheme', () => {
    expect(previewS3Endpoint('httpstore.internal:9000')?.url).toBe(
      'http://httpstore.internal:9000'
    )
  })

  test('IPv6 literals are detected', () => {
    expect(previewS3Endpoint('[fd00::5]:9000')?.hostIsIp).toBe(true)
  })

  test('empty, unsupported and credential-bearing values have no preview', () => {
    expect(previewS3Endpoint('')).toBeNull()
    expect(previewS3Endpoint('ftp://10.0.0.5')).toBeNull()
    expect(previewS3Endpoint('http://key:hunter2@10.0.0.5:9000')).toBeNull()
  })
})

describe('previewPathStyle', () => {
  test('an IP endpoint forces path style and says why', () => {
    const style = previewPathStyle('10.0.0.5:9000', false)
    expect(style).toEqual({
      forcePathStyle: true,
      reason: 'ip_address_endpoint',
    })
    expect(describePathStyleReason(style.reason)).toContain('IP address')
  })

  test('a managed object store forces path style', () => {
    expect(previewPathStyle('http://rustfs-store:9000', false, true)).toEqual({
      forcePathStyle: true,
      reason: 'managed_object_store',
    })
  })

  test('a hostname keeps the operator choice', () => {
    expect(previewPathStyle('https://s3.example.com', false)).toEqual({
      forcePathStyle: false,
      reason: 'configured',
    })
    expect(
      previewPathStyle('https://s3.example.com', true).forcePathStyle
    ).toBe(true)
  })
})
