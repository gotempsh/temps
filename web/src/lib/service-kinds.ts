// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What kind of service a project member is, and which project pages that
 * kind has. A project's sidebar shows a page when at least one of its
 * services has it, and the page itself covers only those services.
 *
 * The kind comes from the member summary the project already carries
 * (`hosting`, `source_type`, `project_type`), so no extra request is made.
 * Whether a deployed service is a background worker is only known at run
 * time (its image declares no port), so workers count as deployed services
 * here: that can only show a page too many, never hide one.
 */

import type { GroupSectionId } from './console-scope'

export type ServiceKind = 'deployed' | 'static' | 'monitored'

export interface ServiceKindSource {
  hosting?: string | null
  source_type?: string | null
  project_type?: string | null
}

export function serviceKind(service: ServiceKindSource): ServiceKind {
  if (service.hosting === 'external' || service.source_type === 'external')
    return 'monitored'
  if (
    service.project_type === 'static' ||
    service.source_type === 'static_files'
  )
    return 'static'
  return 'deployed'
}

export const SERVICE_KIND_LABEL: Record<ServiceKind, string> = {
  deployed: 'App',
  static: 'Static site',
  monitored: 'Monitored',
}

/**
 * The kinds that have each project page. A page missing here (Overview,
 * Settings) belongs to every project whatever its services.
 *
 * Monitoring-only services are hosted elsewhere: no deployments, domains,
 * variables or databases here, but they send errors, traces, logs and
 * analytics. Static sites deploy and take domains and build variables, but
 * run no process that could use a database.
 */
const SECTION_KINDS: Partial<Record<GroupSectionId, readonly ServiceKind[]>> = {
  deployments: ['deployed', 'static'],
  domains: ['deployed', 'static'],
  variables: ['deployed', 'static'],
  databases: ['deployed'],
  backups: ['deployed'],
}

/** Whether a service of `kind` has the project page `section`. */
export function kindHasSection(
  kind: ServiceKind,
  section: GroupSectionId
): boolean {
  const kinds = SECTION_KINDS[section]
  return kinds === undefined || kinds.includes(kind)
}

/** The services of a project that have the page `section`. */
export function servicesWithSection<T extends ServiceKindSource>(
  services: readonly T[],
  section: GroupSectionId
): T[] {
  return services.filter((service) =>
    kindHasSection(serviceKind(service), section)
  )
}

export interface SectionAvailability {
  /** False when none of the project's services can have this page. */
  applicable: boolean
  /** How many of the project's services have it. */
  count: number
  total: number
}

/**
 * Whether a project page applies to a project, given its services. A
 * project with no services yet keeps every page, so it can be explored
 * and onboarded before anything is added.
 */
export function sectionAvailability(
  services: readonly ServiceKindSource[],
  section: GroupSectionId
): SectionAvailability {
  const total = services.length
  const count = servicesWithSection(services, section).length
  return { applicable: total === 0 || count > 0, count, total }
}
