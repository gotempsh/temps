// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Name of the browser window the GitHub App page opens in. */
export const GITHUB_APP_WINDOW = 'temps-github-app'

/**
 * Where the manifest is posted. `state` must be the one the server issued for
 * this attempt: GitHub returns it to the callback, which refuses a manifest
 * code that does not come with a state the server issued.
 */
export function githubManifestAction(state: string): string {
  return `https://github.com/settings/apps/new?state=${encodeURIComponent(state)}`
}

/** Post the manifest to GitHub, into the browser window named `target`. */
export function submitGitHubManifest(
  manifest: unknown,
  state: string,
  target: string
): void {
  const form = document.createElement('form')
  form.method = 'POST'
  form.action = githubManifestAction(state)
  form.target = target

  const input = document.createElement('input')
  input.type = 'hidden'
  input.name = 'manifest'
  input.value = JSON.stringify(manifest)
  form.appendChild(input)

  document.body.appendChild(form)
  try {
    form.submit()
  } finally {
    document.body.removeChild(form)
  }
}
