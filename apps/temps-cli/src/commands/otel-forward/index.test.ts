// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, beforeAll, test, expect, describe } from 'bun:test'
import {
  parseHeaderEnvPairs,
  maskHeaders,
  sanitizeDestinationForOutput,
  sanitizeTextForOutput,
  sanitizeUrlForOutput,
  validateEndpointUrl,
  resolveEnabledFlag,
  buildCreateDestinationBody,
  buildUpdateDestinationBody,
  buildCreateInstanceDefaultBody,
  buildUpdateInstanceDefaultBody,
  type DestinationResponse,
} from './index.js'

const TEST_ENVIRONMENT = {
  TEMPS_OTEL_TEST_A: '1',
  TEMPS_OTEL_TEST_B: '2',
  TEMPS_OTEL_TEST_DD_KEY: 'synthetic-dd-secret',
  TEMPS_OTEL_TEST_CUSTOM: 'Value=With=Equals',
  TEMPS_OTEL_TEST_ORG: 'synthetic-org',
  TEMPS_OTEL_TEST_AUTH: 'Bearer synthetic-auth-secret',
  TEMPS_OTEL_TEST_SENTINEL: '***',
}

beforeAll(() => Object.assign(process.env, TEST_ENVIRONMENT))
afterAll(() => {
  for (const name of Object.keys(TEST_ENVIRONMENT)) delete process.env[name]
})

describe('parseHeaderEnvPairs', () => {
  const environment = {
    DD_KEY: 'synthetic-dd-secret',
    AUTH_VALUE: 'Bearer synthetic-auth-secret',
    FIRST_VALUE: 'one',
    SECOND_VALUE: 'two',
  }

  test('resolves a single KEY=ENV_VAR pair', () => {
    expect(parseHeaderEnvPairs(['DD-API-KEY=DD_KEY'], environment)).toEqual({
      'DD-API-KEY': 'synthetic-dd-secret',
    })
  })

  test('parses multiple pairs', () => {
    expect(parseHeaderEnvPairs(['a=FIRST_VALUE', 'b=SECOND_VALUE'], environment)).toEqual({
      a: 'one',
      b: 'two',
    })
  })

  test('returns an empty object for undefined input', () => {
    expect(parseHeaderEnvPairs(undefined, environment)).toEqual({})
  })

  test('returns an empty object for an empty array', () => {
    expect(parseHeaderEnvPairs([], environment)).toEqual({})
  })

  test('throws on a pair with no "="', () => {
    expect(() => parseHeaderEnvPairs(['not-a-pair'], environment)).toThrow(
      'Invalid --header-env: expected KEY=ENV_VAR'
    )
  })

  test('throws on an empty header or environment name', () => {
    expect(() => parseHeaderEnvPairs(['=DD_KEY'], environment)).toThrow(
      'Invalid --header-env: expected KEY=ENV_VAR'
    )
    expect(() => parseHeaderEnvPairs(['DD-API-KEY='], environment)).toThrow(
      'Invalid --header-env: expected KEY=ENV_VAR'
    )
  })

  test('throws without exposing a missing environment variable value', () => {
    expect(() => parseHeaderEnvPairs(['Authorization=MISSING_SECRET'], environment)).toThrow(
      'Environment variable MISSING_SECRET is not set'
    )
  })
})

describe('resolveEnabledFlag', () => {
  test('returns undefined when neither flag is passed', () => {
    expect(resolveEnabledFlag({})).toBeUndefined()
  })

  test('returns true when --enabled is passed', () => {
    expect(resolveEnabledFlag({ enabled: true })).toBe(true)
  })

  test('returns false when --disabled is passed', () => {
    expect(resolveEnabledFlag({ disabled: true })).toBe(false)
  })

  test('--disabled takes precedence if somehow both are set', () => {
    expect(resolveEnabledFlag({ enabled: true, disabled: true })).toBe(false)
  })
})

describe('buildCreateDestinationBody', () => {
  const baseOptions = {
    projectId: '1',
    name: 'datadog-prod',
    vendor: 'datadog',
    endpointUrl: 'https://otlp.datadoghq.com',
  }

  test('includes only the required fields when nothing else is passed', () => {
    expect(buildCreateDestinationBody(1, baseOptions)).toEqual({
      project_id: 1,
      name: 'datadog-prod',
      vendor_preset: 'datadog',
      endpoint_url: 'https://otlp.datadoghq.com',
    })
  })

  test('includes headers only when at least one --header-env was passed', () => {
    const body = buildCreateDestinationBody(1, {
      ...baseOptions,
      headerEnv: [
        'DD-API-KEY=TEMPS_OTEL_TEST_DD_KEY',
        'X-Custom=TEMPS_OTEL_TEST_CUSTOM',
      ],
    })
    expect(body.headers).toEqual({
      'DD-API-KEY': 'synthetic-dd-secret',
      'X-Custom': 'Value=With=Equals',
    })
  })

  test('omits headers when --header-env was never passed', () => {
    const body = buildCreateDestinationBody(1, baseOptions)
    expect(body.headers).toBeUndefined()
  })

  test('includes forward_traces/metrics/logs only when explicitly set', () => {
    const body = buildCreateDestinationBody(1, { ...baseOptions, traces: false, logs: true })
    expect(body.forward_traces).toBe(false)
    expect(body.forward_logs).toBe(true)
    expect(body.forward_metrics).toBeUndefined()
  })

  test('sets enabled from --disabled', () => {
    const body = buildCreateDestinationBody(1, { ...baseOptions, disabled: true })
    expect(body.enabled).toBe(false)
  })

  test('omits enabled when neither --enabled nor --disabled passed', () => {
    const body = buildCreateDestinationBody(1, baseOptions)
    expect(body.enabled).toBeUndefined()
  })

  test('sets allow_private_network only when the flag is passed', () => {
    expect(buildCreateDestinationBody(1, baseOptions).allow_private_network).toBeUndefined()
    expect(
      buildCreateDestinationBody(1, { ...baseOptions, allowPrivateNetwork: true }).allow_private_network
    ).toBe(true)
  })
})

describe('buildUpdateDestinationBody', () => {
  test('returns an empty object when no fields are passed', () => {
    expect(buildUpdateDestinationBody({})).toEqual({})
  })

  test('includes only the fields that were actually passed', () => {
    expect(buildUpdateDestinationBody({ name: 'renamed' })).toEqual({ name: 'renamed' })
    expect(buildUpdateDestinationBody({ endpointUrl: 'https://example.com' })).toEqual({
      endpoint_url: 'https://example.com',
    })
    expect(buildUpdateDestinationBody({ vendor: 'honeycomb' })).toEqual({ vendor_preset: 'honeycomb' })
  })

  test('includes headers only when --header-env was passed at least once', () => {
    expect(buildUpdateDestinationBody({}).headers).toBeUndefined()
    expect(buildUpdateDestinationBody({ headerEnv: [] }).headers).toBeUndefined()
    expect(
      buildUpdateDestinationBody({ headerEnv: ['a=TEMPS_OTEL_TEST_A'] }).headers
    ).toEqual({ a: '1' })
  })

  test('a header value of exactly "***" round-trips from the environment', () => {
    expect(
      buildUpdateDestinationBody({ headerEnv: ['token=TEMPS_OTEL_TEST_SENTINEL'] }).headers
    ).toEqual({ token: '***' })
  })

  test('includes forward_traces/metrics/logs only when explicitly set', () => {
    const body = buildUpdateDestinationBody({ traces: true })
    expect(body.forward_traces).toBe(true)
    expect(body.forward_metrics).toBeUndefined()
    expect(body.forward_logs).toBeUndefined()
  })

  test('includes enabled only when --enabled or --disabled was passed', () => {
    expect(buildUpdateDestinationBody({}).enabled).toBeUndefined()
    expect(buildUpdateDestinationBody({ enabled: true }).enabled).toBe(true)
    expect(buildUpdateDestinationBody({ disabled: true }).enabled).toBe(false)
  })

  test('includes allow_private_network only when explicitly set (true or false)', () => {
    expect(buildUpdateDestinationBody({}).allow_private_network).toBeUndefined()
    expect(buildUpdateDestinationBody({ allowPrivateNetwork: true }).allow_private_network).toBe(true)
    expect(buildUpdateDestinationBody({ allowPrivateNetwork: false }).allow_private_network).toBe(false)
  })

  test('builds a full partial body with every kind of field mixed', () => {
    const body = buildUpdateDestinationBody({
      name: 'new-name',
      headerEnv: ['a=TEMPS_OTEL_TEST_A', 'b=TEMPS_OTEL_TEST_B'],
      logs: false,
      disabled: true,
      allowPrivateNetwork: true,
    })
    expect(body).toEqual({
      name: 'new-name',
      headers: { a: '1', b: '2' },
      forward_logs: false,
      enabled: false,
      allow_private_network: true,
    })
  })
})

describe('buildCreateInstanceDefaultBody', () => {
  const baseOptions = {
    name: 'instance-openobserve',
    vendor: 'generic_otlp',
    endpointUrl: 'https://openobserve.internal',
  }

  test('includes only the required fields when nothing else is passed, with no project_id', () => {
    const body = buildCreateInstanceDefaultBody(baseOptions)
    expect(body).toEqual({
      name: 'instance-openobserve',
      vendor_preset: 'generic_otlp',
      endpoint_url: 'https://openobserve.internal',
    })
    expect('project_id' in body).toBe(false)
  })

  test('includes headers only when at least one --header-env was passed', () => {
    const body = buildCreateInstanceDefaultBody({
      ...baseOptions,
      headerEnv: [
        'X-Org=TEMPS_OTEL_TEST_ORG',
        'Authorization=TEMPS_OTEL_TEST_AUTH',
      ],
    })
    expect(body.headers).toEqual({
      'X-Org': 'synthetic-org',
      Authorization: 'Bearer synthetic-auth-secret',
    })
  })

  test('includes forward_traces/metrics/logs only when explicitly set', () => {
    const body = buildCreateInstanceDefaultBody({ ...baseOptions, metrics: false })
    expect(body.forward_metrics).toBe(false)
    expect(body.forward_traces).toBeUndefined()
    expect(body.forward_logs).toBeUndefined()
  })

  test('sets enabled from --disabled', () => {
    expect(buildCreateInstanceDefaultBody({ ...baseOptions, disabled: true }).enabled).toBe(false)
  })

  test('sets allow_private_network only when the flag is passed', () => {
    expect(buildCreateInstanceDefaultBody(baseOptions).allow_private_network).toBeUndefined()
    expect(
      buildCreateInstanceDefaultBody({ ...baseOptions, allowPrivateNetwork: true }).allow_private_network
    ).toBe(true)
  })
})

describe('buildUpdateInstanceDefaultBody', () => {
  test('returns an empty object when no fields are passed', () => {
    expect(buildUpdateInstanceDefaultBody({})).toEqual({})
  })

  test('includes only the fields that were actually passed, with no project_id ever present', () => {
    const body = buildUpdateInstanceDefaultBody({ endpointUrl: 'https://example.com' })
    expect(body).toEqual({ endpoint_url: 'https://example.com' })
    expect('project_id' in body).toBe(false)
  })

  test('a header value of exactly "***" round-trips from the environment', () => {
    expect(
      buildUpdateInstanceDefaultBody({
        headerEnv: ['x-api-key=TEMPS_OTEL_TEST_SENTINEL'],
      }).headers
    ).toEqual({ 'x-api-key': '***' })
  })

  test('includes allow_private_network only when explicitly set (true or false)', () => {
    expect(buildUpdateInstanceDefaultBody({}).allow_private_network).toBeUndefined()
    expect(buildUpdateInstanceDefaultBody({ allowPrivateNetwork: false }).allow_private_network).toBe(false)
  })

  test('builds a full partial body with every kind of field mixed', () => {
    const body = buildUpdateInstanceDefaultBody({
      name: 'renamed-default',
      headerEnv: ['a=TEMPS_OTEL_TEST_A'],
      traces: false,
      enabled: true,
    })
    expect(body).toEqual({
      name: 'renamed-default',
      headers: { a: '1' },
      forward_traces: false,
      enabled: true,
    })
  })
})

describe('output sanitization', () => {
  test('masks every header value, including vendor-specific and arbitrary names', () => {
    expect(
      maskHeaders({
        'x-honeycomb-team': 'synthetic-honeycomb-secret',
        'X-Org': 'synthetic-client-identifier',
        Authorization: 'Bearer synthetic-auth-secret',
      })
    ).toEqual({
      'x-honeycomb-team': '***',
      'X-Org': '***',
      Authorization: '***',
    })
  })

  test('redacts URL credentials, all query values, and fragments', () => {
    const sanitized = sanitizeUrlForOutput(
      'https://user:synthetic-password@collector.example.com/otlp?tenant=synthetic-client&sig=synthetic-signature#private'
    )
    expect(sanitized).toContain('collector.example.com/otlp')
    expect(sanitized).not.toContain('user')
    expect(sanitized).not.toContain('synthetic-password')
    expect(sanitized).not.toContain('synthetic-client')
    expect(sanitized).not.toContain('synthetic-signature')
    expect(sanitized).not.toContain('private')
  })

  test('sanitizes URLs embedded in error text', () => {
    const sanitized = sanitizeTextForOutput(
      'delivery to https://user:synthetic-password@example.com/v1/traces?sig=synthetic-signature failed'
    )
    expect(sanitized).toContain('delivery to https://example.com/v1/traces')
    expect(sanitized).not.toContain('synthetic-password')
    expect(sanitized).not.toContain('synthetic-signature')
  })

  test('sanitizes destination objects used by both human and JSON output', () => {
    const destination: DestinationResponse = {
      id: 1,
      project_id: 2,
      name: 'synthetic-destination',
      vendor_preset: 'honeycomb',
      endpoint_url: 'https://collector.example.com/otlp?tenant=synthetic-client',
      headers: { 'x-honeycomb-team': 'synthetic-honeycomb-secret', 'X-Org': 'synthetic-client' },
      forward_traces: true,
      forward_metrics: true,
      forward_logs: true,
      enabled: true,
      allow_private_network: false,
      last_success_at: null,
      last_error_at: '2026-08-10T12:00:00Z',
      last_error: 'request to https://user:synthetic-password@example.com/?sig=synthetic-signature failed',
      consecutive_failures: 1,
      status: 'failing',
      created_at: '2026-08-10T10:00:00Z',
      updated_at: '2026-08-10T12:00:00Z',
    }

    const serialized = JSON.stringify(sanitizeDestinationForOutput(destination))
    expect(serialized).toContain('x-honeycomb-team')
    expect(serialized).toContain('***')
    expect(serialized).not.toContain('synthetic-honeycomb-secret')
    expect(serialized).not.toContain('synthetic-client')
    expect(serialized).not.toContain('synthetic-password')
    expect(serialized).not.toContain('synthetic-signature')
  })
})

describe('endpoint URL validation', () => {
  test('allows credential-free HTTP(S) collector URLs', () => {
    expect(validateEndpointUrl('https://collector.example.com/otlp')).toBe(
      'https://collector.example.com/otlp'
    )
  })

  test('rejects userinfo without echoing it', () => {
    expect(() =>
      validateEndpointUrl('https://user:synthetic-password@collector.example.com/otlp')
    ).toThrow('URL credentials are not allowed')
  })

  test('rejects query parameters and fragments', () => {
    expect(() => validateEndpointUrl('https://collector.example.com/otlp?sig=synthetic-secret')).toThrow(
      'query parameters are not allowed'
    )
    expect(() => validateEndpointUrl('https://collector.example.com/otlp#synthetic-secret')).toThrow(
      'fragments are not allowed'
    )
  })

  test('rejects non-HTTP and malformed URLs', () => {
    expect(() => validateEndpointUrl('file:///tmp/collector')).toThrow(
      'only HTTP(S) URLs are supported'
    )
    expect(() => validateEndpointUrl('not-a-url')).toThrow(
      'expected an absolute HTTP(S) URL'
    )
  })
})
