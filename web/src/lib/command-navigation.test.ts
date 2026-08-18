import { describe, expect, test } from 'bun:test'
import {
  buildCommandSampleQueries,
  dedupeCommandDestinations,
  resolveExplicitProjectEnvironment,
  resolveExplicitNamedProjectDestination,
  toCommandExtendedQuery,
  type CommandDestination,
} from './command-navigation'

const destination = (
  id: string,
  url: string,
  title = id
): CommandDestination => ({
  id,
  url,
  title,
  description: title,
  category: 'Test',
  keywords: [],
})

describe('command destination catalog', () => {
  test('keeps the first description for duplicate routes', () => {
    expect(
      dedupeCommandDestinations([
        destination('sidebar', '/backups', 'Backups'),
        destination('settings', '/backups', 'Backup settings'),
      ])
    ).toEqual([destination('sidebar', '/backups', 'Backups')])
  })

  test('does not collapse the same page across different projects', () => {
    const destinations = [
      destination(
        'alpha-production',
        '/projects/alpha/environments?environment=production'
      ),
      destination(
        'beta-production',
        '/projects/beta/environments?environment=production'
      ),
    ]
    expect(dedupeCommandDestinations(destinations)).toEqual(destinations)
  })

  test('treats a written project slug and production environment as constraints', () => {
    const destinations = [
      destination(
        'project:alpha:environment:production',
        '/projects/alpha/environments?environment=production'
      ),
      destination(
        'project:marketing-site:environment:production',
        '/projects/marketing-site/environments?environment=production'
      ),
    ]

    expect(
      resolveExplicitProjectEnvironment(
        'environment production marketing-site',
        ['alpha', 'marketing-site'],
        destinations
      )?.url
    ).toBe('/projects/marketing-site/environments?environment=production')
    expect(
      resolveExplicitProjectEnvironment(
        'production environment',
        ['alpha'],
        destinations
      )
    ).toBeUndefined()
    expect(
      resolveExplicitProjectEnvironment(
        'production environment for this project',
        ['alpha', 'marketing-site'],
        destinations,
        'alpha'
      )?.url
    ).toBe('/projects/alpha/environments?environment=production')

    expect(
      resolveExplicitProjectEnvironment(
        'environment prod api',
        ['marketing-site', 'api-control-plane'],
        destinations.concat(
          destination(
            'project:api-control-plane:environment:production',
            '/projects/api-control-plane/environments?environment=production'
          )
        )
      )?.url
    ).toBe('/projects/api-control-plane/environments?environment=production')
  })

  test('does not guess when a partial project reference is ambiguous', () => {
    const destinations = [
      destination(
        'project:api-app:environment:production',
        '/projects/api-app/environments?environment=production'
      ),
      destination(
        'project:api-worker:environment:production',
        '/projects/api-worker/environments?environment=production'
      ),
    ]

    expect(
      resolveExplicitProjectEnvironment(
        'environment prod api',
        ['api-app', 'api-worker'],
        destinations
      )
    ).toBeUndefined()
  })

  test('turns natural language into an AND query without filler words', () => {
    expect(toCommandExtendedQuery('Show me the postgres service')).toBe(
      "'postgres 'service"
    )
    expect(toCommandExtendedQuery('backups S3')).toBe("'backups 's3")
  })

  test('keeps an explicitly named project when using ranked local intent', () => {
    const ranked = [
      destination(
        'project:marketing-site:analytics/visitors',
        '/projects/marketing-site/analytics/visitors'
      ),
      destination(
        'project:api-control-plane:analytics/visitors',
        '/projects/api-control-plane/analytics/visitors'
      ),
    ]

    expect(
      resolveExplicitNamedProjectDestination(
        'show visitors for api-control-plane',
        ['marketing-site', 'api-control-plane'],
        ranked
      )?.url
    ).toBe('/projects/api-control-plane/analytics/visitors')
  })

  test('builds examples from real projects and services', () => {
    expect(
      buildCommandSampleQueries({
        projectSlugs: ['marketing-site', 'api-control-plane'],
        services: [{ name: 'primary-db', serviceType: 'postgres' }],
      })
    ).toEqual([
      'Show me the latest S3 backups',
      'Open the PostgreSQL service primary-db',
      'Open the production environment for marketing-site',
      'Show visitors for api-control-plane',
    ])
  })

  test('uses useful collection pages when resources do not exist yet', () => {
    expect(
      buildCommandSampleQueries({ projectSlugs: [], services: [] })
    ).toEqual([
      'Show me the latest S3 backups',
      'Show me all database services',
      'Show me all projects',
      'Open platform monitoring',
    ])
  })
})
