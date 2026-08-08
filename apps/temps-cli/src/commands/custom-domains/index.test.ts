import { test, expect, describe } from 'bun:test'
import { buildCustomDomainUpdateBody } from './index.js'

describe('buildCustomDomainUpdateBody', () => {
  test('produces an empty body when no options are provided', () => {
    expect(buildCustomDomainUpdateBody({})).toEqual({})
  })

  test('includes only the fields that were actually passed', () => {
    expect(buildCustomDomainUpdateBody({ domain: 'new.example.com' })).toEqual({
      domain: 'new.example.com',
    })
  })

  test('parses environment id and status code to numbers', () => {
    const body = buildCustomDomainUpdateBody({ environmentId: '3', statusCode: '301' })
    expect(body).toEqual({ environment_id: 3, status_code: 301 })
  })

  test('clears branch to null on an explicit empty string rather than dropping it', () => {
    // Distinguishes "unset this field" (empty string) from "leave it alone"
    // (flag omitted) — omitted fields never appear in the body at all.
    expect(buildCustomDomainUpdateBody({ branch: '' })).toEqual({ branch: null })
    expect(buildCustomDomainUpdateBody({})).toEqual({})
  })

  test('clears redirect_to to null on an explicit empty string', () => {
    expect(buildCustomDomainUpdateBody({ redirectTo: '' })).toEqual({ redirect_to: null })
  })

  test('passes a real branch and redirect target through untouched', () => {
    const body = buildCustomDomainUpdateBody({ branch: 'main', redirectTo: 'https://example.com' })
    expect(body).toEqual({ branch: 'main', redirect_to: 'https://example.com' })
  })

  test('combines every field when all options are supplied', () => {
    const body = buildCustomDomainUpdateBody({
      domain: 'new.example.com',
      environmentId: '2',
      branch: 'staging',
      redirectTo: 'https://target.example.com',
      statusCode: '308',
    })
    expect(body).toEqual({
      domain: 'new.example.com',
      environment_id: 2,
      branch: 'staging',
      redirect_to: 'https://target.example.com',
      status_code: 308,
    })
  })
})
