import { test, expect, describe } from 'bun:test'
import {
  isLikelySecret,
  detectServiceTypeFromKey,
  detectServiceTypeFromUrl,
  inferPreset,
  buildSteps,
  buildSummary,
} from './base.js'
import type { EnvVarPlan, ServicePlan, DomainPlan, ProjectPlan } from '../types.js'

// ---------------------------------------------------------------------------
// isLikelySecret
// ---------------------------------------------------------------------------

describe('isLikelySecret', () => {
  test('detects common secret key patterns', () => {
    expect(isLikelySecret('DATABASE_PASSWORD')).toBe(true)
    expect(isLikelySecret('API_KEY')).toBe(true)
    expect(isLikelySecret('JWT_SECRET')).toBe(true)
    expect(isLikelySecret('AUTH_TOKEN')).toBe(true)
    expect(isLikelySecret('ACCESS_KEY_ID')).toBe(true)
    expect(isLikelySecret('PRIVATE_KEY')).toBe(true)
    expect(isLikelySecret('ENCRYPTION_KEY')).toBe(true)
    expect(isLikelySecret('SESSION_SECRET')).toBe(true)
    expect(isLikelySecret('COOKIE_SECRET')).toBe(true)
    expect(isLikelySecret('SIGNING_KEY')).toBe(true)
  })

  test('does not flag non-secret keys', () => {
    expect(isLikelySecret('PORT')).toBe(false)
    expect(isLikelySecret('NODE_ENV')).toBe(false)
    expect(isLikelySecret('DATABASE_URL')).toBe(false)
    expect(isLikelySecret('APP_NAME')).toBe(false)
    expect(isLikelySecret('LOG_LEVEL')).toBe(false)
  })
})

// ---------------------------------------------------------------------------
// detectServiceTypeFromKey
// ---------------------------------------------------------------------------

describe('detectServiceTypeFromKey', () => {
  test('detects postgres', () => {
    expect(detectServiceTypeFromKey('POSTGRES_URL')).toBe('postgres')
    expect(detectServiceTypeFromKey('PG_HOST')).toBe('postgres')
    expect(detectServiceTypeFromKey('DATABASE_URL')).toBe('postgres')
  })

  test('detects redis', () => {
    expect(detectServiceTypeFromKey('REDIS_URL')).toBe('redis')
    expect(detectServiceTypeFromKey('REDIS_HOST')).toBe('redis')
  })

  test('detects mongodb', () => {
    expect(detectServiceTypeFromKey('MONGODB_URI')).toBe('mongodb')
    expect(detectServiceTypeFromKey('MONGO_URL')).toBe('mongodb')
  })

  test('detects s3', () => {
    expect(detectServiceTypeFromKey('S3_BUCKET')).toBe('s3')
    expect(detectServiceTypeFromKey('AWS_BUCKET_NAME')).toBe('s3')
    expect(detectServiceTypeFromKey('MINIO_ENDPOINT')).toBe('s3')
  })

  test('returns null for unknown', () => {
    expect(detectServiceTypeFromKey('PORT')).toBe(null)
    expect(detectServiceTypeFromKey('NODE_ENV')).toBe(null)
  })
})

// ---------------------------------------------------------------------------
// detectServiceTypeFromUrl
// ---------------------------------------------------------------------------

describe('detectServiceTypeFromUrl', () => {
  test('detects postgres URLs', () => {
    expect(detectServiceTypeFromUrl('postgres://user:pass@host:5432/db')).toBe('postgres')
    expect(detectServiceTypeFromUrl('postgresql://user:pass@host/db')).toBe('postgres')
  })

  test('detects redis URLs', () => {
    expect(detectServiceTypeFromUrl('redis://host:6379')).toBe('redis')
    expect(detectServiceTypeFromUrl('rediss://host:6380')).toBe('redis')
  })

  test('detects mongodb URLs', () => {
    expect(detectServiceTypeFromUrl('mongodb://host:27017/db')).toBe('mongodb')
    expect(detectServiceTypeFromUrl('mongodb+srv://host/db')).toBe('mongodb')
  })

  test('detects s3 URLs', () => {
    expect(detectServiceTypeFromUrl('https://bucket.s3.amazonaws.com')).toBe('s3')
    expect(detectServiceTypeFromUrl('http://minio:9000')).toBe('s3')
  })

  test('returns null for unknown URLs', () => {
    expect(detectServiceTypeFromUrl('https://example.com')).toBe(null)
    expect(detectServiceTypeFromUrl('http://api.stripe.com')).toBe(null)
  })
})

// ---------------------------------------------------------------------------
// inferPreset
// ---------------------------------------------------------------------------

describe('inferPreset', () => {
  test('infers nextjs preset', () => {
    expect(inferPreset('Next.js')).toBe('nextjs')
    expect(inferPreset('nextjs')).toBe('nextjs')
  })

  test('infers static preset for static frameworks', () => {
    expect(inferPreset('astro')).toBe('static')
    expect(inferPreset('gatsby')).toBe('static')
    expect(inferPreset('hugo')).toBe('static')
    expect(inferPreset('create-react-app')).toBe('static')
  })

  test('infers docker preset from build type', () => {
    expect(inferPreset(undefined, 'dockerfile')).toBe('docker')
    expect(inferPreset('docker')).toBe('docker')
  })

  test('defaults to nixpacks', () => {
    expect(inferPreset()).toBe('nixpacks')
    expect(inferPreset(undefined, undefined)).toBe('nixpacks')
    expect(inferPreset('nuxt')).toBe('nixpacks')
    expect(inferPreset('express')).toBe('nixpacks')
    expect(inferPreset('python')).toBe('nixpacks')
    expect(inferPreset('ruby')).toBe('nixpacks')
  })
})

// ---------------------------------------------------------------------------
// buildSteps
// ---------------------------------------------------------------------------

describe('buildSteps', () => {
  test('creates project step first', () => {
    const steps = buildSteps({
      project: { name: 'test', preset: 'nixpacks', directory: '.', mainBranch: 'main' },
      envVars: [],
      services: [],
      domains: [],
    })

    expect(steps.length).toBe(1)
    expect(steps[0]!.id).toBe('create-project')
    expect(steps[0]!.order).toBe(1)
    expect(steps[0]!.skippable).toBe(false)
  })

  test('adds service steps before env vars', () => {
    const steps = buildSteps({
      project: { name: 'test', preset: 'nixpacks', directory: '.', mainBranch: 'main' },
      envVars: [{ key: 'FOO', value: 'bar', isSecret: false, skip: false }],
      services: [
        {
          name: 'pg',
          type: 'postgres',
          action: 'create',
          actionDescription: 'Create postgres',
          envVarKeys: [],
          dataImplications: [],
        },
      ],
      domains: [],
    })

    expect(steps.length).toBe(3) // project + service + env-vars
    expect(steps[1]!.id).toBe('service-pg')
    expect(steps[2]!.id).toBe('set-env-vars')
  })

  test('adds git step when git info present', () => {
    const steps = buildSteps({
      project: {
        name: 'test',
        preset: 'nixpacks',
        directory: '.',
        mainBranch: 'main',
        git: { provider: 'github', owner: 'user', repo: 'repo', defaultBranch: 'main' },
      },
      envVars: [],
      services: [],
      domains: [],
    })

    expect(steps.some((s) => s.id === 'configure-git')).toBe(true)
  })

  test('adds domain steps after git', () => {
    const steps = buildSteps({
      project: { name: 'test', preset: 'nixpacks', directory: '.', mainBranch: 'main' },
      envVars: [],
      services: [],
      domains: [{ domain: 'example.com', action: 'import', actionDescription: 'Import domain' }],
    })

    const domainStep = steps.find((s) => s.id === 'domain-example.com')
    expect(domainStep).toBeDefined()
    expect(domainStep!.risk).toBe('medium')
  })

  test('skips services with action=skip', () => {
    const steps = buildSteps({
      project: { name: 'test', preset: 'nixpacks', directory: '.', mainBranch: 'main' },
      envVars: [],
      services: [
        {
          name: 'pg',
          type: 'postgres',
          action: 'skip',
          actionDescription: 'Skip',
          envVarKeys: [],
          dataImplications: [],
        },
      ],
      domains: [],
    })

    expect(steps.some((s) => s.id === 'service-pg')).toBe(false)
  })

  test('skips env vars step when all are skipped', () => {
    const steps = buildSteps({
      project: { name: 'test', preset: 'nixpacks', directory: '.', mainBranch: 'main' },
      envVars: [{ key: 'FOO', value: 'bar', isSecret: false, skip: true }],
      services: [],
      domains: [],
    })

    expect(steps.some((s) => s.id === 'set-env-vars')).toBe(false)
  })
})

// ---------------------------------------------------------------------------
// buildSummary
// ---------------------------------------------------------------------------

describe('buildSummary', () => {
  test('calculates counts correctly', () => {
    const summary = buildSummary(
      'vercel',
      {
        envVars: [
          { key: 'A', value: '1', isSecret: false, skip: false },
          { key: 'B', value: '2', isSecret: false, skip: true },
        ],
        services: [
          { name: 'pg', type: 'postgres', action: 'create', actionDescription: '', envVarKeys: [], dataImplications: [] },
        ],
        domains: [
          { domain: 'example.com', action: 'import', actionDescription: '' },
        ],
      },
      []
    )

    expect(summary.counts.envVars).toBe(1) // only non-skipped
    expect(summary.counts.services).toBe(1)
    expect(summary.counts.domains).toBe(1)
  })

  test('sets overall risk to high when data services have data-not-migrated', () => {
    const summary = buildSummary(
      'vercel',
      {
        envVars: [],
        services: [
          {
            name: 'pg',
            type: 'postgres',
            action: 'create',
            actionDescription: '',
            envVarKeys: [],
            dataImplications: [{ severity: 'data-not-migrated', message: 'Data not migrated' }],
          },
        ],
        domains: [],
      },
      []
    )

    expect(summary.overallRisk).toBe('high')
    expect(summary.criticalWarnings.some((w) => w.includes('NOT automatically migrated'))).toBe(true)
  })

  test('sets overall risk to medium when domains are present', () => {
    const summary = buildSummary(
      'vercel',
      {
        envVars: [],
        services: [],
        domains: [{ domain: 'example.com', action: 'import', actionDescription: '' }],
      },
      []
    )

    expect(summary.overallRisk).toBe('medium')
    expect(summary.criticalWarnings.some((w) => w.includes('DNS'))).toBe(true)
  })

  test('sets overall risk to none when nothing risky', () => {
    const summary = buildSummary(
      'vercel',
      {
        envVars: [{ key: 'A', value: '1', isSecret: false, skip: false }],
        services: [],
        domains: [],
      },
      []
    )

    expect(summary.overallRisk).toBe('none')
  })

  test('adds manual actions for data services', () => {
    const summary = buildSummary(
      'vercel',
      {
        envVars: [],
        services: [
          {
            name: 'pg',
            type: 'postgres',
            action: 'create',
            actionDescription: '',
            envVarKeys: [],
            dataImplications: [{ severity: 'data-not-migrated', message: 'Data not migrated' }],
          },
        ],
        domains: [],
      },
      []
    )

    expect(summary.manualActions.some((a) => a.timing === 'before' && a.description.includes('Export'))).toBe(true)
    expect(summary.manualActions.some((a) => a.timing === 'after' && a.description.includes('Import'))).toBe(true)
  })

  test('notes unsupported features in warnings', () => {
    const summary = buildSummary(
      'vercel',
      { envVars: [], services: [], domains: [] },
      [{ feature: 'Edge Functions', reason: 'Not supported' }]
    )

    expect(summary.criticalWarnings.some((w) => w.includes('1 feature'))).toBe(true)
  })
})
