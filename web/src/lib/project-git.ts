// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Whether a project actually deploys from a Git repository.
 *
 * `repo_owner`/`repo_name` are NOT NULL columns, and project creation fills
 * them with the literal string `"unknown"` when no repository was supplied
 * (`crates/temps-projects/src/services/project.rs`). So "has a repo" cannot be
 * a truthiness check — a project created from static files, an uploaded
 * archive or a Docker image carries `unknown/unknown` and looks configured.
 *
 * The backend already applies exactly this rule before allowing a switch to
 * `source_type = git`; this mirrors it for the UI so both agree on what
 * "connected to Git" means.
 *
 * Getting it wrong is visible: the environment settings page rendered a branch
 * selector for such a project, which fetched
 * `/api/git/public/github/unknown/unknown/branches`, got a 404, and showed
 * "Error Loading Branches — Failed to load branches from repository" on a
 * project that had no repository to load branches from in the first place.
 */
const PLACEHOLDER_REPO_FIELD = 'unknown'

export interface ProjectGitFields {
  repo_owner?: string | null
  repo_name?: string | null
  git_url?: string | null
  git_provider_connection_id?: number | null
}

function isRealRepoField(value: string | null | undefined): boolean {
  const trimmed = value?.trim() ?? ''
  return trimmed !== '' && trimmed !== PLACEHOLDER_REPO_FIELD
}

export function projectHasGitRepo(project: ProjectGitFields): boolean {
  // A provider connection or an explicit clone URL is decisive on its own: a
  // connected project may legitimately not have picked a repository yet, and
  // that is an onboarding state, not "this project isn't a Git project".
  if (project.git_provider_connection_id) return true
  if ((project.git_url?.trim() ?? '') !== '') return true

  // Otherwise it's a public repo, identified only by owner + name — both must
  // be real for the branch API to have anything to query.
  return (
    isRealRepoField(project.repo_owner) && isRealRepoField(project.repo_name)
  )
}
