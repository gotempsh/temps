// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * User-facing names for the two levels of the project hierarchy.
 *
 * The API keeps its original vocabulary: a `project` there is one deployable
 * app, and a `project group` gathers several. In the console that reads as
 * a Project (the group) containing Services (the deployable apps). Every
 * surface that names either level imports from here, so the wording can be
 * changed in one place while the naming is still an experiment.
 */
export const TERMS = {
  /** A `project_group`: several services that ship together. */
  group: { singular: 'Project', plural: 'Projects' },
  /** A `project`: one deployable app. */
  service: { singular: 'Service', plural: 'Services' },
} as const

export function serviceCount(n: number): string {
  return `${n} ${n === 1 ? 'service' : 'services'}`
}

/** Console route of a project group's overview. */
export function groupHref(slug: string): string {
  return `/p/${slug}`
}

/** Search parameter of the connect-repository page naming a follow-up step. */
export const THEN_PARAM = 'then'
/** `?then=import`: continue into importing services once connected. */
export const IMPORT_AFTER_CONNECT = 'import'
/** Search parameter of a project's overview that opens "From the repository". */
export const IMPORT_PARAM = 'import'

/** Console route for connecting (or changing) a project's repository. */
export function groupConnectRepositoryHref(
  slug: string,
  options: { thenImport?: boolean } = {}
): string {
  const base = `${groupHref(slug)}/repository`
  return options.thenImport
    ? `${base}?${THEN_PARAM}=${IMPORT_AFTER_CONNECT}`
    : base
}
