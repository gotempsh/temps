// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Prefix of the name of the browser window the GitHub App page opens in. */
export const GITHUB_APP_WINDOW = 'temps-github-app'

/**
 * A window name for one flow instance. Windows are shared by name, so a flow
 * that is opened again while an earlier one is still setting up must not reuse
 * (and so be able to close) the earlier one's window.
 */
export function githubAppWindowName(): string {
  return `${GITHUB_APP_WINDOW}-${Math.random().toString(36).slice(2, 10)}`
}

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
    /**
     * Stop whatever attempt is in flight (Back, another method, leaving) and
     * close the blank window it opened, unless a manifest is already in it.
     */
    cancel(): void {
      current = 0
      if (!holdsManifest) github?.close()
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

/**
 * Open (or reuse) the named window GitHub's App page loads in, without giving
 * that page a reference back to the console.
 *
 * `window.open` with a name gives the new window an `opener`, unlike the
 * `target="_blank"` form this replaced, which browsers open as `noopener`.
 * Passing `noopener` would also drop the handle the setup needs to post the
 * manifest and close a blank window, so the opener is cleared instead while
 * the window is still a same-origin blank page. It stays cleared when the
 * window navigates to GitHub. A window that is already cross-origin is
 * returned as it is: its opener was cleared when it was opened.
 */
export function openGithubAppWindow<W extends { opener: unknown }>(
  name: string,
  open: (url: string, target: string) => W | null
): W | null {
  const githubWindow = open('', name)
  if (githubWindow) {
    try {
      githubWindow.opener = null
    } catch {
      // A window that is already on GitHub is cross-origin, and its `opener`
      // cannot be written from here: assigning it throws a SecurityError.
      // That window had its opener cleared when it was first opened, while
      // it was still a blank page, so there is nothing left to cut. Failing
      // here would stop every later attempt until the window was closed.
    }
  }
  return githubWindow
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
