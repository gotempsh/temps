// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, mock, beforeEach, afterEach } from 'bun:test'
import { VercelAdapter } from './vercel.js'
import type { ProjectSnapshot, PlatformCredentials } from '../types.js'

// ---------------------------------------------------------------------------
// generatePlan tests (pure function — no HTTP mocking needed)
// ---------------------------------------------------------------------------

describe('VercelAdapter.generatePlan', () => {
  const adapter = new VercelAdapter()

  function makeSnapshot(overrides: Partial<ProjectSnapshot> = {}): ProjectSnapshot {
    return {
      id: 'prj_123',
      name: 'my-nextjs-app',
      framework: 'Next.js',
      git: {
        provider: 'github',
        owner: 'myorg',
        repo: 'my-nextjs-app',
        defaultBranch: 'main',
        cloneUrl: 'https://github.com/myorg/my-nextjs-app.git',
      },
      envVars: [
        { key: 'NODE_ENV', value: 'production', isSecret: false, source: 'vercel:plain' },
        { key: 'DATABASE_URL', value: 'postgres://user:pass@host:5432/db', isSecret: true, source: 'vercel:secret' },
        { key: 'API_KEY', value: 'sk-1234', isSecret: true, source: 'vercel:secret' },
      ],
      services: [
        {
          id: 'detected-postgres',
          name: 'postgres',
          type: 'postgres',
          hasData: true,
          envVarKeys: ['DATABASE_URL'],
          connectionUrl: 'postgres://user:pass@host:5432/db',
        },
      ],
      domains: [
        { domain: 'myapp.com', isApex: true },
        { domain: 'www.myapp.com', isApex: false },
      ],
      build: {
        type: 'Next.js',
        buildCommand: 'next build',
        installCommand: 'npm install',
        outputDirectory: '.next',
      },
      ...overrides,
    }
  }

  test('generates a plan with correct platform', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    expect(plan.platform).toBe('vercel')
    expect(plan.sourceProjectId).toBe('prj_123')
    expect(plan.sourceProjectName).toBe('my-nextjs-app')
  })

  test('infers nextjs preset for Next.js framework', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    expect(plan.project.preset).toBe('nextjs')
  })

  test('maps git info to project plan', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    expect(plan.project.git).toBeDefined()
    expect(plan.project.git!.provider).toBe('github')
    expect(plan.project.git!.owner).toBe('myorg')
    expect(plan.project.git!.repo).toBe('my-nextjs-app')
    expect(plan.project.mainBranch).toBe('main')
  })

  test('maps build info', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    expect(plan.project.buildCommand).toBe('next build')
    expect(plan.project.installCommand).toBe('npm install')
    expect(plan.project.outputDir).toBe('.next')
  })

  test('marks service-related env vars as skip', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    const dbUrl = plan.envVars.find((e) => e.key === 'DATABASE_URL')
    expect(dbUrl).toBeDefined()
    expect(dbUrl!.skip).toBe(true) // Managed by postgres service
  })

  test('does not skip non-service env vars', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    const nodeEnv = plan.envVars.find((e) => e.key === 'NODE_ENV')
    expect(nodeEnv).toBeDefined()
    expect(nodeEnv!.skip).toBe(false)
  })

  test('creates service plan with data-not-migrated implication', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    expect(plan.services.length).toBe(1)
    expect(plan.services[0]!.type).toBe('postgres')
    expect(plan.services[0]!.action).toBe('create')
    expect(plan.services[0]!.dataImplications.some((d) => d.severity === 'data-not-migrated')).toBe(true)
  })

  test('creates domain plans', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    expect(plan.domains.length).toBe(2)
    expect(plan.domains[0]!.domain).toBe('myapp.com')
    expect(plan.domains[0]!.action).toBe('import')
  })

  test('detects unsupported Vercel features', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    expect(plan.unsupportedFeatures.length).toBeGreaterThan(0)
    expect(plan.unsupportedFeatures.some((f) => f.feature.includes('Edge'))).toBe(true)
    expect(plan.unsupportedFeatures.some((f) => f.feature.includes('Analytics'))).toBe(true)
  })

  test('builds ordered execution steps', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    expect(plan.steps.length).toBeGreaterThan(0)
    expect(plan.steps[0]!.id).toBe('create-project')

    // Service should come before env vars
    const serviceIdx = plan.steps.findIndex((s) => s.id.startsWith('service-'))
    const envVarsIdx = plan.steps.findIndex((s) => s.id === 'set-env-vars')
    if (serviceIdx !== -1 && envVarsIdx !== -1) {
      expect(serviceIdx).toBeLessThan(envVarsIdx)
    }
  })

  test('summary reflects plan contents', () => {
    const plan = adapter.generatePlan(makeSnapshot())
    expect(plan.summary.counts.services).toBe(1)
    expect(plan.summary.counts.domains).toBe(2)
    expect(plan.summary.overallRisk).toBe('high') // Due to data-not-migrated
    expect(plan.summary.manualActions.length).toBeGreaterThan(0)
  })

  test('handles project with no git info', () => {
    const plan = adapter.generatePlan(makeSnapshot({ git: undefined }))
    expect(plan.project.git).toBeUndefined()
    expect(plan.steps.some((s) => s.id === 'configure-git')).toBe(false)
  })

  test('handles project with no services', () => {
    const plan = adapter.generatePlan(makeSnapshot({ services: [] }))
    expect(plan.services.length).toBe(0)
    expect(plan.steps.some((s) => s.id.startsWith('service-'))).toBe(false)
  })

  test('handles project with no domains', () => {
    const plan = adapter.generatePlan(makeSnapshot({ domains: [] }))
    expect(plan.domains.length).toBe(0)
  })

  test('handles project with no env vars', () => {
    const plan = adapter.generatePlan(makeSnapshot({ envVars: [] }))
    expect(plan.envVars.length).toBe(0)
    expect(plan.steps.some((s) => s.id === 'set-env-vars')).toBe(false)
  })

  test('detects Vercel managed services from env vars', () => {
    const plan = adapter.generatePlan(
      makeSnapshot({
        envVars: [
          { key: 'BLOB_READ_WRITE_TOKEN', value: 'vercel_blob_token', isSecret: true, source: 'vercel:secret' },
          { key: 'KV_REST_API_URL', value: 'https://kv.vercel.com', isSecret: false, source: 'vercel:plain' },
        ],
        services: [],
      })
    )

    expect(plan.unsupportedFeatures.some((f) => f.feature.includes('Vercel managed services'))).toBe(true)
  })
})
