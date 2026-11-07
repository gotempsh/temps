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

/** The part of a browser window the setup needs. */
interface SetupWindow {
  closed: boolean
  close(): void
}

/**
 * Follows GitHub App setup attempts. The state is requested from the server
 * before the manifest is posted, so the operator can go Back, pick another
 * method or leave while that request is in flight, or start again before the
 * first request has answered. This tells each attempt whether it may still
 * carry on, and whether it may close GitHub's window, which is shared by name
 * between attempts.
 */
export function createManifestSetupTracker<W extends SetupWindow>() {
  let latest = 0
  let current = 0
  let owner = 0
  let github: W | null = null
  let holdsManifest = false

  return {
    /**
     * Start an attempt and open (or reuse) GitHub's window for it. Any earlier
     * attempt stops being current and no longer owns the window.
     */
    begin(openWindow: () => W | null): { attempt: number; window: W | null } {
      const windowStillOpen = github !== null && !github.closed
      if (!windowStillOpen) holdsManifest = false
      latest += 1
      current = latest
      owner = latest
      github = openWindow()
      return { attempt: latest, window: github }
    },
    /** Stop whatever attempt is in flight (Back, another method, leaving). */
    cancel(): void {
      current = 0
    },
    /** Whether the attempt may still post its manifest and watch for the app. */
    isCurrent: (attempt: number): boolean => attempt === current,
    /**
     * Close GitHub's window for an attempt that stops. Only the attempt that
     * owns it may, and not once a manifest has been posted into it: that
     * window is where the operator is finishing the setup.
     */
    closeWindowFor(attempt: number): void {
      if (attempt === owner && !holdsManifest) github?.close()
    },
    /** The manifest is now in GitHub's window. */
    manifestPosted(): void {
      holdsManifest = true
    },
  }
}

/** Whether a window returned by `window.open` can still receive the manifest. */
export function githubWindowUsable(
  githubWindow: { closed: boolean } | null
): boolean {
  return githubWindow !== null && !githubWindow.closed
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
