import { describe, expect, test } from 'bun:test'
import type { DeploymentResponse } from '@/api/client'
import { resolvePrimaryUrl } from './deployment-url'

type DeploymentOverrides = Omit<Partial<DeploymentResponse>, 'environment'> & {
  environment?: Partial<DeploymentResponse['environment']>
}

function deployment(overrides: DeploymentOverrides = {}): DeploymentResponse {
  const { environment, ...rest } = overrides
  return {
    created_at: 0,
    environment_id: 2,
    id: 3,
    is_current: false,
    project_id: 2,
    status: 'completed',
    url: 'http://observability-starter-1.127.0.0.1.sslip.io',
    environment: {
      domains: [],
      id: 2,
      name: 'production',
      slug: 'production',
      ...environment,
    },
    ...rest,
  }
}

describe('resolvePrimaryUrl', () => {
  test('current deployment prefers the stable environment domain', () => {
    // Regression: the project header used to link to `deployment.url`, whose
    // slug-derived host (`{project}-{n}`) is ephemeral and, on a sslip.io
    // install, does not even resolve to the right IP.
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          environment: {
            domains: [
              'http://observability-starter-production.127.0.0.1.sslip.io',
            ],
          },
        })
      )
    ).toBe('http://observability-starter-production.127.0.0.1.sslip.io')
  })

  test('custom domain configured on the environment wins over the slug host', () => {
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          environment: { domains: ['https://app.example.com'] },
        })
      )
    ).toBe('https://app.example.com')
  })

  test('superseded deployment keeps its own deployment-specific URL', () => {
    // Old builds are not served at the environment domain, so linking there
    // would silently show the user a different version than they asked for.
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: false,
          environment: {
            domains: [
              'http://observability-starter-production.127.0.0.1.sslip.io',
            ],
          },
        })
      )
    ).toBe('http://observability-starter-1.127.0.0.1.sslip.io')
  })

  test('current deployment without env domains falls back to its own URL', () => {
    expect(resolvePrimaryUrl(deployment({ is_current: true }))).toBe(
      'http://observability-starter-1.127.0.0.1.sslip.io'
    )
  })

  test('bare hostnames are normalized to absolute URLs', () => {
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          environment: { domains: ['app.example.com'] },
        })
      )
    ).toBe('https://app.example.com')
  })

  test('returns null when there is nothing to link to', () => {
    expect(resolvePrimaryUrl(deployment({ url: '' }))).toBeNull()
  })

  test('empty deployment URL still falls back to the environment domain', () => {
    expect(
      resolvePrimaryUrl(
        deployment({ url: '', environment: { domains: ['app.example.com'] } })
      )
    ).toBe('https://app.example.com')
  })
})
