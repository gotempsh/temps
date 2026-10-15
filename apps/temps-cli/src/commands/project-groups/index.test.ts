// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import type { GroupEnvVarResponse, GroupRouteResponse, ProjectGroupMemberResponse } from '../../api/types.gen.js'
import { runWithApi } from '../../lib/mock-api.js'
import {
  caseHint,
  describeInherited,
  describeMembership,
  failedRepoSync,
  fetchAllPages,
  registerProjectGroupsCommands,
  stripFinalNewline,
  importedApp,
  describeRouteEnvironments,
  findMember,
  findVariable,
  normalizeDirectory,
  normalizeScope,
  parseRepoRef,
  resolveRepositorySource,
  suggestServiceName,
} from './index.js'

function variable(key: string, environment_slug: string | null): GroupEnvVarResponse {
  return {
    id: key.length + (environment_slug?.length ?? 0),
    group_id: 1,
    key,
    value: '***',
    is_secret: false,
    environment_slug,
    include_in_preview: true,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z'
  } as GroupEnvVarResponse
}

describe('normalizeScope', () => {
  test('treats a missing, empty or "all" environment as every environment', () => {
    expect(normalizeScope()).toBeNull()
    expect(normalizeScope('  ')).toBeNull()
    expect(normalizeScope('ALL')).toBeNull()
  })

  test('lowercases and trims an environment slug', () => {
    expect(normalizeScope(' Production ')).toBe('production')
  })
})

describe('findVariable', () => {
  const vars = [variable('LOG_LEVEL', null), variable('LOG_LEVEL', 'production')]

  test('matches key and scope together', () => {
    expect(findVariable(vars, 'LOG_LEVEL', null)?.environment_slug).toBeNull()
    expect(findVariable(vars, 'LOG_LEVEL', 'production')?.environment_slug).toBe('production')
  })

  test('does not fall back to another scope', () => {
    expect(findVariable(vars, 'LOG_LEVEL', 'staging')).toBeUndefined()
    expect(findVariable(vars, 'API_URL', null)).toBeUndefined()
  })
})

describe('parseRepoRef', () => {
  test('splits owner and name, keeping subgroup owners', () => {
    expect(parseRepoRef('acme/shop')).toEqual({ owner: 'acme', name: 'shop' })
    expect(parseRepoRef('acme/platform/shop.git')).toEqual({ owner: 'acme/platform', name: 'shop' })
  })

  test('rejects values that are not owner/name', () => {
    for (const bad of ['shop', '/shop', 'acme/']) {
      expect(() => parseRepoRef(bad)).toThrow()
    }
  })
})

describe('monorepo import helpers', () => {
  test('normalizeDirectory treats ./, . and empty as the root', () => {
    expect(normalizeDirectory('./apps/api/')).toBe('apps/api')
    expect(normalizeDirectory('./')).toBe('.')
  })

  test('suggestServiceName uses the folder, suffixing taken names', () => {
    expect(suggestServiceName('./services/worker', 'shop', new Set())).toBe('worker')
    expect(suggestServiceName('./', 'shop', new Set(['shop']))).toBe('shop-2')
  })

  test('importedApp builds the service from its directory of the repository', () => {
    const app = importedApp(
      { path: './apps/api', preset: 'dockerfile', presetLabel: 'Dockerfile', projectType: 'backend', dockerfilePath: 'apps/api/Dockerfile', exposedPort: 8080 },
      'api'
    )
    expect(app).toEqual({
      name: 'api',
      directory: 'apps/api',
      preset: 'dockerfile',
      exposed_port: 8080,
      preset_config: { dockerfilePath: 'apps/api/Dockerfile' },
    })
    expect(importedApp({ path: './', preset: 'nextjs', presetLabel: 'Next.js', projectType: 'frontend' }, 'web').directory).toBe('.')
  })
})

describe('resolveRepositorySource', () => {
  test('a connection id reads the repository through that connection', () => {
    expect(resolveRepositorySource({ connection: '3', repo: 'acme/shop' })).toEqual({
      connectionId: 3,
      owner: 'acme',
      name: 'shop',
    })
  })

  test('a github.com URL implies a public repository', () => {
    expect(
      resolveRepositorySource({ repo: 'https://github.com/Infisical/infisical/' })
    ).toEqual({ publicProvider: 'github', owner: 'Infisical', name: 'infisical' })
    expect(resolveRepositorySource({ repo: 'gitlab.com/acme/shop.git' })).toMatchObject({
      publicProvider: 'gitlab',
    })
  })

  test('--public names the provider for owner/name', () => {
    expect(resolveRepositorySource({ public: 'GitHub', repo: 'acme/shop' })).toMatchObject({
      publicProvider: 'github',
    })
  })

  test('rejects ambiguous or unusable sources', () => {
    expect(() => resolveRepositorySource({ repo: 'acme/shop' })).toThrow(/--connection/)
    expect(() => resolveRepositorySource({ connection: '3', public: 'github', repo: 'a/b' })).toThrow(
      /not both/
    )
    expect(() => resolveRepositorySource({ public: 'gitea', repo: 'a/b' })).toThrow(/github or gitlab/)
    expect(() => resolveRepositorySource({ repo: 'https://git.example.com/a/b' })).toThrow(
      /github.com and gitlab.com/
    )
    expect(() => resolveRepositorySource({ connection: 'x', repo: 'a/b' })).toThrow(/connection id/)
  })
})

describe('routes', () => {
  const member = (id: number, slug: string, name: string) =>
    ({ id, slug, name }) as ProjectGroupMemberResponse
  const group = { name: 'Shop', projects: [member(1, 'web', 'Web'), member(2, 'api', 'API')] }

  test('finds a service by id, slug or name', () => {
    expect(findMember(group, '2').id).toBe(2)
    expect(findMember(group, 'web').id).toBe(1)
    expect(findMember(group, 'api').id).toBe(2)
    expect(() => findMember(group, 'worker')).toThrow('services: web, api')
  })

  test('describes each environment pairing', () => {
    const route = {
      environments: [
        { source_environment_id: 1, source_environment_slug: 'production', target_environment_id: 3, target_environment_slug: 'production', target_deployed: true },
        { source_environment_id: 2, source_environment_slug: 'staging', target_environment_id: 4, target_environment_slug: 'staging', target_deployed: false },
        { source_environment_id: 5, source_environment_slug: 'preview-x', target_environment_id: null, target_environment_slug: null, target_deployed: false },
      ],
    } as GroupRouteResponse
    expect(describeRouteEnvironments(route)).toBe(
      'production → production, staging → staging (not deployed), preview-x → (no matching environment)'
    )
  })
})

const shop = {
  id: 9,
  name: 'Shop',
  slug: 'shop',
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  projects: [] as ProjectGroupMemberResponse[],
}

function service(id: number, slug: string, repo?: { owner: string; name: string; directory: string }) {
  return {
    id,
    name: slug,
    slug,
    repo_owner: repo?.owner ?? null,
    repo_name: repo?.name ?? null,
    directory: repo?.directory ?? './',
  }
}

describe('helpers', () => {
  test('failedRepoSync keeps only the services that were not updated', () => {
    const result = {
      services: [
        { project_id: 1, name: 'web', slug: 'web' },
        { project_id: 2, name: 'api', slug: 'api', error: 'branch not found' },
      ],
    }
    expect(failedRepoSync(result).map((s) => s.slug)).toEqual(['api'])
  })

  test('fetchAllPages reads every page up to the total', async () => {
    const all = Array.from({ length: 250 }, (_, i) => i)
    const pages: number[] = []
    const items = await fetchAllPages(async (page, perPage) => {
      pages.push(page)
      return { items: all.slice((page - 1) * perPage, page * perPage), total: all.length }
    })
    expect(items).toEqual(all)
    expect(pages).toEqual([1, 2, 3])
  })

  test('fetchAllPages stops on an empty page and refuses a list that never ends', async () => {
    expect(await fetchAllPages(async () => ({ items: [], total: 10 }))).toEqual([])
    await expect(fetchAllPages(async () => ({ items: [1], total: 1_000_000 }), 1, 3)).rejects.toThrow('did not end')
  })

  test('variable names are sent as typed, with a hint when not upper case', () => {
    expect(caseHint('LOG_LEVEL')).toBeUndefined()
    expect(caseHint('log_level')).toContain('case-sensitive')
  })

  test('a piped value loses only the final line break', () => {
    expect(stripFinalNewline('secret\n')).toBe('secret')
    expect(stripFinalNewline('secret\r\n')).toBe('secret')
    expect(stripFinalNewline(' a\nb \n\n')).toBe(' a\nb \n')
  })

  test('describes membership and inherited variables', () => {
    expect(describeMembership(null)).toContain('standalone')
    expect(describeMembership({ ...shop })).toBe('Shop (shop)')
    expect(
      describeInherited({ key: 'A', is_secret: true, include_in_preview: false, defined_on_project: true, environment_slug: null })
    ).toEqual({ scope: 'all environments', value: '(secret)', previews: 'no', effect: "overridden by the service's own variable" })
    expect(
      describeInherited({ key: 'A', is_secret: false, include_in_preview: true, defined_on_project: false, environment_slug: 'production' })
        .previews
    ).toBe('—')
  })
})

describe('repo set', () => {
  test('--json still exits non-zero when a service could not be updated', async () => {
    const run = await runWithApi(
      registerProjectGroupsCommands,
      ['project-groups', 'repo', 'set', 'shop', '--connection', '3', '--repo', 'acme/shop', '--branch', 'main', '--json'],
      {
        'GET /project-groups/by-slug/shop': shop,
        'PUT /project-groups/9/repository': {
          group: shop,
          services: [
            { project_id: 1, name: 'web', slug: 'web' },
            { project_id: 2, name: 'api', slug: 'api', error: 'branch main not found' },
          ],
        },
      }
    )
    expect(run.error).toBeUndefined()
    expect(run.exitCode).toBe(1)
    expect(JSON.parse(run.stdout).services[1].error).toBe('branch main not found')
  })

  test('exits zero when every service was updated', async () => {
    const run = await runWithApi(
      registerProjectGroupsCommands,
      ['project-groups', 'repo', 'set', 'shop', '--connection', '3', '--repo', 'acme/shop', '--branch', 'main', '--json'],
      {
        'GET /project-groups/by-slug/shop': shop,
        'PUT /project-groups/9/repository': { group: shop, services: [{ project_id: 1, name: 'web', slug: 'web' }] },
      }
    )
    expect(run.exitCode).toBe(0)
  })
})

describe('import', () => {
  test('checks every service, not just the first page, for apps that already have one', async () => {
    const firstPage = Array.from({ length: 100 }, (_, i) => service(i + 1, `other-${i + 1}`))
    const run = await runWithApi(
      registerProjectGroupsCommands,
      ['project-groups', 'import', 'shop', '--dry-run', '--json'],
      {
        'GET /project-groups/by-slug/shop': shop,
        'GET /project-groups/9/repository': {
          repository: { repo_owner: 'acme', repo_name: 'shop', main_branch: 'main', git_provider_connection_id: 3, repository_id: 5 },
        },
        'GET /repositories/5/preset/live': {
          repository_id: 5,
          owner: 'acme',
          name: 'shop',
          calculated_at: '',
          presets: [
            { path: './apps/web', preset: 'nextjs', presetLabel: 'Next.js', projectType: 'frontend' },
            { path: './apps/api', preset: 'dockerfile', presetLabel: 'Dockerfile', projectType: 'backend' },
          ],
        },
        'GET /projects': (url: URL) =>
          url.searchParams.get('page') === '2'
            ? { projects: [service(101, 'api', { owner: 'acme', name: 'shop', directory: 'apps/api' })], page: 2, per_page: 100, total: 101 }
            : { projects: firstPage, page: 1, per_page: 100, total: 101 },
      }
    )
    expect(run.error).toBeUndefined()
    expect(run.requests.filter((r) => r.startsWith('GET /projects?'))).toHaveLength(2)
    // apps/api is deployed as `api` (found on page 2), so only apps/web is new.
    expect(JSON.parse(run.stdout)).toEqual([{ directory: 'apps/web', preset: 'nextjs', name: 'web', existing: null }])
  })
})

describe('vars', () => {
  const routes = {
    'GET /project-groups/by-slug/shop': shop,
    'GET /project-groups/9/env-vars': [],
    'POST /project-groups/9/env-vars': (_url: URL, body: unknown) => ({
      id: 1,
      group_id: 9,
      created_at: '',
      updated_at: '',
      ...(body as object),
    }),
  }

  test('set keeps the name as typed and leaves previews out unless asked', async () => {
    const run = await runWithApi(
      registerProjectGroupsCommands,
      ['project-groups', 'vars', 'set', 'shop', 'log_level', 'debug', '--json'],
      routes
    )
    expect(run.error).toBeUndefined()
    const sent = run.bodies.find((b) => b.request === 'POST /project-groups/9/env-vars')?.body as Record<string, unknown>
    expect(sent.key).toBe('log_level')
    expect(sent.include_in_preview).toBe(false)
    expect(run.stderr).toContain('case-sensitive')
  })

  test('--preview opts an all-environments variable into previews', async () => {
    const run = await runWithApi(
      registerProjectGroupsCommands,
      ['project-groups', 'vars', 'set', 'shop', 'LOG_LEVEL', 'debug', '--preview', '--json'],
      routes
    )
    const sent = run.bodies.find((b) => b.request === 'POST /project-groups/9/env-vars')?.body as Record<string, unknown>
    expect(sent.include_in_preview).toBe(true)
  })

  test('a value and --value-stdin together are refused before anything is sent', async () => {
    const run = await runWithApi(
      registerProjectGroupsCommands,
      ['project-groups', 'vars', 'set', 'shop', 'API_KEY', 'x', '--value-stdin'],
      routes
    )
    expect(run.error?.message).toContain('not both')
    expect(run.requests).toEqual([])
  })

  test('a missing value without a terminal is an error, not an empty variable', async () => {
    const tty = process.stdin.isTTY
    process.stdin.isTTY = false
    try {
      const run = await runWithApi(registerProjectGroupsCommands, ['project-groups', 'vars', 'set', 'shop', 'API_KEY'], routes)
      expect(run.error?.message).toContain('--value-stdin')
      expect(run.bodies).toEqual([])
    } finally {
      process.stdin.isTTY = tty
    }
  })

  test('inherited lists what a service receives from its project', async () => {
    const inherited = {
      group: { id: 9, name: 'Shop', slug: 'shop' },
      variables: [{ key: 'LOG_LEVEL', is_secret: false, include_in_preview: false, defined_on_project: true }],
    }
    const run = await runWithApi(registerProjectGroupsCommands, ['project-groups', 'vars', 'inherited', 'web', '--json'], {
      'GET /projects/by-slug/web': service(4, 'web'),
      'GET /projects/4/inherited-env-vars': inherited,
    })
    expect(run.error).toBeUndefined()
    expect(JSON.parse(run.stdout)).toEqual(inherited)
  })
})
