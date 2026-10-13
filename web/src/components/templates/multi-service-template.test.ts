// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  EnvVarTemplateResponse,
  InstantiationResponse,
  TemplateResponse,
  TemplateServiceResponse,
} from '@/api/client'
import {
  buildInstantiateRequest,
  compatibleDatabases,
  currentStepIndex,
  defaultServiceNames,
  failedStep,
  initialVariables,
  isMultiServiceTemplate,
  missingVariableIndexes,
  serviceSourceLabel,
  slugifyProjectName,
  validateMultiServiceForm,
  type MultiServiceFormState,
} from './multi-service-template'

function envVar(
  name: string,
  extra: Partial<EnvVarTemplateResponse> = {}
): EnvVarTemplateResponse {
  return { name, required: false, secret: false, ...extra }
}

function service(
  name: string,
  extra: Partial<TemplateServiceResponse> = {}
): TemplateServiceResponse {
  return {
    name,
    preset: 'nodejs',
    databases: [],
    database_bindings: {},
    env_vars: [],
    ...extra,
  }
}

function template(extra: Partial<TemplateResponse> = {}): TemplateResponse {
  return {
    slug: 'shop',
    name: 'Shop',
    kind: 'starter',
    preset: 'nextjs',
    databases: ['postgres'],
    services: ['postgres'],
    env_vars: [],
    features: [],
    tags: [],
    is_featured: false,
    git: { url: 'https://github.com/temps/shop', ref: 'main' },
    managed_service_bindings: {},
    routes: [{ prefix: '/api', source: 'web', target: 'api' }],
    service_definitions: [
      service('web', {
        env_vars: [
          envVar('NEXT_PUBLIC_URL', { default_generator: 'app_url' }),
          envVar('STRIPE_KEY', { required: true }),
        ],
      }),
      service('api', {
        directory: 'api',
        database_bindings: { postgres: { DATABASE_URL: 'url' } },
        env_vars: [
          envVar('DATABASE_URL', { required: true }),
          envVar('JWT_SECRET', {
            required: true,
            default_generator: 'random_secret',
          }),
        ],
      }),
    ],
    shared_env_vars: [
      envVar('APP_URL', { default_generator: 'app_url' }),
      envVar('SESSION_SECRET', {
        required: true,
        default_generator: 'random_secret',
      }),
      envVar('SUPPORT_EMAIL', { required: true }),
    ],
    version: '1',
    ...extra,
  }
}

const base = { scheme: 'https' as const, host: 'example.com' }

function state(t: TemplateResponse): MultiServiceFormState {
  const serviceNames = defaultServiceNames(t, slugifyProjectName('My Shop'))
  return {
    target: 'new_project',
    projectName: 'My Shop',
    groupId: null,
    serviceNames,
    variables: initialVariables(t, serviceNames, base),
    databases: [{ type: 'postgres', mode: 'create', serviceId: null }],
  }
}

describe('slugifyProjectName', () => {
  test('matches the server slug of a project name', () => {
    expect(slugifyProjectName('My  Shop!')).toBe('my-shop')
    expect(slugifyProjectName('--Shop--')).toBe('shop')
    expect(slugifyProjectName('Ünïcode 2')).toBe('n-code-2')
    expect(slugifyProjectName('!!!')).toBe('')
  })
})

describe('isMultiServiceTemplate', () => {
  test('is true only for several services', () => {
    expect(isMultiServiceTemplate(template())).toBe(true)
    expect(
      isMultiServiceTemplate(
        template({ service_definitions: [service('web')] })
      )
    ).toBe(false)
  })
})

describe('defaultServiceNames', () => {
  test('prefixes each template service with the project slug', () => {
    expect(defaultServiceNames(template(), 'my-shop')).toEqual([
      { templateService: 'web', name: 'my-shop-web' },
      { templateService: 'api', name: 'my-shop-api' },
    ])
  })
})

describe('initialVariables', () => {
  test('runs generators, using each service slug for app_url', () => {
    const vars = state(template()).variables
    const byKey = new Map(vars.map((v) => [`${v.service}:${v.name}`, v]))
    expect(byKey.get('web:NEXT_PUBLIC_URL')?.value).toBe(
      'https://my-shop-web-production.example.com'
    )
    // Shared app_url follows the service receiving the project's hostnames.
    expect(byKey.get('null:APP_URL')?.value).toBe(
      'https://my-shop-web-production.example.com'
    )
    expect(byKey.get('api:JWT_SECRET')?.value.length).toBeGreaterThan(20)
    expect(byKey.get('api:JWT_SECRET')?.isSecret).toBe(true)
    expect(byKey.get('web:STRIPE_KEY')?.value).toBe('')
  })
})

describe('missingVariableIndexes', () => {
  test('flags required values the server cannot fill', () => {
    const t = template()
    const s = state(t)
    const missing = missingVariableIndexes(t, s.variables).map(
      (i) => `${s.variables[i].service}:${s.variables[i].name}`
    )
    // DATABASE_URL comes from the linked database; generated values count.
    expect(missing).toEqual(['web:STRIPE_KEY', 'null:SUPPORT_EMAIL'])
  })

  test('a cleared shared app_url needs a value; a cleared random secret does not', () => {
    const t = template()
    const s = state(t)
    const variables = s.variables.map((v) =>
      v.name === 'APP_URL' ||
      v.name === 'SESSION_SECRET' ||
      v.name === 'JWT_SECRET'
        ? { ...v, value: '' }
        : v
    )
    const missing = missingVariableIndexes(t, variables).map(
      (i) => variables[i].name
    )
    expect(missing).toContain('APP_URL')
    expect(missing).not.toContain('SESSION_SECRET')
    expect(missing).not.toContain('JWT_SECRET')
  })
})

describe('validateMultiServiceForm', () => {
  test('reports missing target, duplicate names, values and databases', () => {
    const t = template()
    const s = state(t)
    const issues = validateMultiServiceForm(t, {
      ...s,
      target: 'existing_project',
      groupId: null,
      serviceNames: [
        { templateService: 'web', name: 'same' },
        { templateService: 'api', name: 'Same' },
      ],
      databases: [{ type: 'postgres', mode: 'existing', serviceId: null }],
    })
    const paths = issues.map((issue) => issue.path.join('.'))
    expect(paths).toContain('groupId')
    expect(paths).toContain('serviceNames.1.name')
    expect(paths).toContain('databases.0.serviceId')
    expect(paths.filter((p) => p.startsWith('variables.'))).toHaveLength(2)
  })

  test('accepts a complete form', () => {
    const t = template()
    const s = state(t)
    const variables = s.variables.map((v) =>
      v.value ? v : { ...v, value: 'x' }
    )
    expect(validateMultiServiceForm(t, { ...s, variables })).toEqual([])
  })
})

describe('buildInstantiateRequest', () => {
  test('builds the instantiate body', () => {
    const t = template()
    const s = state(t)
    const variables = s.variables.map((v) =>
      v.name === 'STRIPE_KEY' ? { ...v, value: 'sk_test' } : v
    )
    const body = buildInstantiateRequest(
      {
        ...s,
        variables,
        databases: [{ type: 'postgres', mode: 'existing', serviceId: 9 }],
      },
      'key-1'
    )
    expect(body.target).toEqual({ type: 'new_project', name: 'My Shop' })
    expect(body.service_names).toEqual({
      web: 'my-shop-web',
      api: 'my-shop-api',
    })
    expect(body.databases).toEqual({
      postgres: { type: 'existing', service_id: 9 },
    })
    expect(body.idempotency_key).toBe('key-1')
    const stripe = body.environment_variables?.find(
      (v) => v.name === 'STRIPE_KEY'
    )
    expect(stripe).toEqual({
      name: 'STRIPE_KEY',
      value: 'sk_test',
      is_secret: false,
      service: 'web',
    })
    // Shared variables carry no service; empty values are not sent.
    const shared = body.environment_variables?.find((v) => v.name === 'APP_URL')
    expect(shared?.service).toBeUndefined()
    expect(
      body.environment_variables?.some((v) => v.name === 'SUPPORT_EMAIL')
    ).toBe(false)
  })

  test('targets an existing project by id', () => {
    const t = template()
    const body = buildInstantiateRequest(
      { ...state(t), target: 'existing_project', groupId: 4 },
      'k'
    )
    expect(body.target).toEqual({ type: 'existing_project', group_id: 4 })
    expect(body.databases).toEqual({ postgres: { type: 'create' } })
  })
})

describe('compatibleDatabases', () => {
  test('matches aliases of the database type', () => {
    const services = [
      { id: 1, service_type: 'postgres' },
      { id: 2, service_type: 'redis' },
      { id: 3, service_type: 'PostgreSQL' },
    ]
    expect(
      compatibleDatabases('postgresql', services).map((s) => s.id)
    ).toEqual([1, 3])
  })
})

describe('serviceSourceLabel', () => {
  test('describes images and source builds', () => {
    expect(serviceSourceLabel({ image: 'nginx:1' })).toBe('Image nginx:1')
    expect(serviceSourceLabel({ directory: './api' })).toBe(
      'Builds from source in api'
    )
    expect(serviceSourceLabel({ directory: '.' })).toBe('Builds from source')
  })
})

describe('steps', () => {
  const run = (
    status: InstantiationResponse['status'],
    steps: InstantiationResponse['steps']
  ) => ({ status, steps })

  test('the current step is the first pending one while running', () => {
    const steps = [
      { key: 'a', label: 'A', status: 'done' as const },
      { key: 'b', label: 'B', status: 'pending' as const },
      { key: 'c', label: 'C', status: 'pending' as const },
    ]
    expect(currentStepIndex(run('running', steps))).toBe(1)
    expect(currentStepIndex(run('failed', steps))).toBe(-1)
  })

  test('finds the failed step', () => {
    expect(
      failedStep({
        steps: [
          { key: 'a', label: 'A', status: 'done' },
          { key: 'b', label: 'B', status: 'failed', error: 'boom' },
        ],
      })?.error
    ).toBe('boom')
  })
})
