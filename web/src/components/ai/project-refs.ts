// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The few fields of a project that matching and the chip need. */
export interface ProjectRef {
  id: number
  slug: string
  name: string
  service_template_image_url?: string | null
  source_type?: string | null
}

/**
 * The project a piece of inline code names, if it names exactly one.
 *
 * A slug is unique, so an exact slug wins. A display name is matched ignoring
 * case, but only when a single project carries it: two projects called the
 * same would make the chip open the wrong one. Anything else (a partial name,
 * a sentence, a command) is not a reference, so ordinary code stays code.
 */
export function matchProjectRef(
  text: string,
  projects: readonly ProjectRef[]
): ProjectRef | null {
  const wanted = text.trim()
  if (!wanted) return null
  const bySlug = projects.find((project) => project.slug === wanted)
  if (bySlug) return bySlug
  const lower = wanted.toLowerCase()
  const byName = projects.filter(
    (project) =>
      project.name.toLowerCase() === lower ||
      project.slug.toLowerCase() === lower
  )
  return byName.length === 1 ? byName[0] : null
}
