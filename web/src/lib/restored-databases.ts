// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  MissingLinkedDatabase,
  RestoredDatabase,
  RestoredDatabasesReport,
} from '@/api/client/types.gen'

/** One linked project whose database the backup does not have. */
export type MissingProject = {
  projectId: number
  projectName: string
  projectSlug: string
  /** What each environment (or the whole project) reads today. */
  expected: { environment: string | null; database: string }[]
  /** Environments that would all read one database after the change. */
  environmentNames: string[]
  suggested: string | null
}

/** Group the missing databases by project, in report order. */
export function missingProjects(
  missing: MissingLinkedDatabase[]
): MissingProject[] {
  const byProject = new Map<number, MissingProject>()
  for (const entry of missing) {
    const existing = byProject.get(entry.project_id)
    const expected = {
      environment: entry.environment_name ?? null,
      database: entry.expected_database,
    }
    if (existing) {
      existing.expected.push(expected)
      existing.suggested ??= entry.suggested_database ?? null
    } else {
      byProject.set(entry.project_id, {
        projectId: entry.project_id,
        projectName: entry.project_name,
        projectSlug: entry.project_slug,
        expected: [expected],
        environmentNames: entry.environment_names,
        suggested: entry.suggested_database ?? null,
      })
    }
  }
  return [...byProject.values()]
}

/** Restored databases no linked project reads that a link can point at. */
export function candidateDatabases(report: RestoredDatabasesReport): string[] {
  return report.databases
    .filter((database) => database.kind === 'unused' && database.usable_by_link)
    .map((database) => database.name)
}

/** The database preselected for a project: the suggestion, else the first. */
export function defaultChoice(
  project: MissingProject,
  candidates: string[]
): string | null {
  if (project.suggested && candidates.includes(project.suggested)) {
    return project.suggested
  }
  return candidates[0] ?? null
}

/**
 * Pointing a link at one database makes every environment read it, which
 * mixes data when the project has more than one: ask first.
 */
export function needsConfirmation(project: MissingProject): boolean {
  return project.environmentNames.length > 1
}

/**
 * What the project reads today, after its name: "(production) reads
 * shop_production", "(production, staging) reads shop_production and
 * shop_staging", or "reads shop" for a whole project.
 */
export function expectedLine(project: MissingProject): string {
  const environments = project.expected
    .map(({ environment }) => environment)
    .filter((environment): environment is string => environment !== null)
  const databases = project.expected.map(({ database }) => database)
  const listed =
    databases.length > 1
      ? `${databases.slice(0, -1).join(', ')} and ${databases[databases.length - 1]}`
      : databases.join('')
  return environments.length > 0
    ? `(${environments.join(', ')}) reads ${listed}`
    : `reads ${listed}`
}

/** How a restored database relates to the service, for its row. */
export function databaseRole(database: RestoredDatabase): string {
  switch (database.kind) {
    case 'linked': {
      const readers = database.used_by.map((use) =>
        use.environment_name
          ? `${use.project_name} (${use.environment_name})`
          : use.project_name
      )
      if (database.used_by_other_projects) readers.push('another project')
      return `Read by ${readers.join(', ')}`
    }
    case 'unused':
      return 'No linked project reads it'
    case 'service':
      return "This service's database"
    case 'system':
      return "PostgreSQL's own database"
    case 'sandbox':
      return 'A sandbox database'
  }
}

function plural(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`
}

/** "3 databases and 2 users", with "+" when a list was cut. */
export function restoredSummary(report: RestoredDatabasesReport): string {
  const more = report.truncated ? '+' : ''
  return `${plural(report.databases.length, 'database', 'databases')}${more} and ${plural(
    report.roles.length,
    'user',
    'users'
  )}${more}`
}
