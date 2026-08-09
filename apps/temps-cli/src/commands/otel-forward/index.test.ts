import { test, expect, describe } from 'bun:test'
import {
  parseHeaderPairs,
  resolveEnabledFlag,
  buildCreateDestinationBody,
  buildUpdateDestinationBody,
} from './index.js'

describe('parseHeaderPairs', () => {
  test('parses a single KEY=VALUE pair', () => {
    expect(parseHeaderPairs(['DD-API-KEY=abc123'])).toEqual({ 'DD-API-KEY': 'abc123' })
  })

  test('parses multiple pairs', () => {
    expect(parseHeaderPairs(['a=1', 'b=2'])).toEqual({ a: '1', b: '2' })
  })

  test('only splits on the first "=" — values may contain "="', () => {
    expect(parseHeaderPairs(['Authorization=Bearer abc=def'])).toEqual({
      Authorization: 'Bearer abc=def',
    })
  })

  test('returns an empty object for undefined input', () => {
    expect(parseHeaderPairs(undefined)).toEqual({})
  })

  test('returns an empty object for an empty array', () => {
    expect(parseHeaderPairs([])).toEqual({})
  })

  test('throws on a pair with no "="', () => {
    expect(() => parseHeaderPairs(['not-a-pair'])).toThrow(
      "Invalid --header 'not-a-pair': expected KEY=VALUE"
    )
  })

  test('throws on a pair with an empty key (leading "=")', () => {
    expect(() => parseHeaderPairs(['=value'])).toThrow(
      "Invalid --header '=value': expected KEY=VALUE"
    )
  })

  test('allows an empty value', () => {
    expect(parseHeaderPairs(['key='])).toEqual({ key: '' })
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

  test('includes headers only when at least one --header was passed', () => {
    const body = buildCreateDestinationBody(1, {
      ...baseOptions,
      header: ['DD-API-KEY=abc123', 'X-Custom=Value=With=Equals'],
    })
    expect(body.headers).toEqual({
      'DD-API-KEY': 'abc123',
      'X-Custom': 'Value=With=Equals',
    })
  })

  test('omits headers when --header was never passed', () => {
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

  test('includes headers only when --header was passed at least once', () => {
    expect(buildUpdateDestinationBody({}).headers).toBeUndefined()
    expect(buildUpdateDestinationBody({ header: [] }).headers).toBeUndefined()
    expect(buildUpdateDestinationBody({ header: ['a=1'] }).headers).toEqual({ a: '1' })
  })

  test('a header value of exactly "***" round-trips as a literal value (server interprets the sentinel)', () => {
    expect(buildUpdateDestinationBody({ header: ['token=***'] }).headers).toEqual({ token: '***' })
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
      header: ['a=1', 'b=2'],
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
