// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The commands a new worker runs to join this cluster.
 *
 * They install Temps as root and call it by its absolute path:
 * - `temps join` sets up WireGuard and the overlay network, and
 *   `temps agent service install` writes a systemd unit, so both need root;
 * - the agent service refuses a binary root does not own, so an install into
 *   a user's own `~/.temps` cannot run it;
 * - the installer adds Temps to PATH only for shells started afterwards, and
 *   `sudo` resets PATH anyway, so a bare `temps` is not found on the next line.
 *
 * `sudo -H` sets HOME to root's, so the binary and the agent's data land in
 * `/root/.temps` wherever sudo's own HOME policy differs (the SSH enrolment
 * does the same, see `ROOT_INSTALL_PATH` in `temps-nodes`).
 */

export const INSTALLER_URL = 'https://temps.sh/install.sh'
/** Where the installer puts Temps when root runs it with `sudo -H`. */
export const ROOT_TEMPS_BIN = '/root/.temps/bin/temps'
const AS_ROOT = 'sudo -H'

/**
 * A release tag the installer can download, or `null`.
 *
 * The control plane reports its running version (`v0.1.0`,
 * `v0.1.0-beta.2`, `v0.1.0-nightly.20261104.0123abcde`). Anything else, such
 * as a local development build or an unreadable version, is not pinned: the
 * installer would fail to download it. The pattern is a subset of the
 * installer's own plain-tag check (`[0-9A-Za-z._-]`), so the value is also
 * safe to paste into a shell.
 */
export function releaseTag(version: string | null | undefined): string | null {
  const tag = version?.trim()
  if (!tag) return null
  return /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$/.test(tag)
    ? tag
    : null
}

/**
 * Install Temps as root. With a tag it installs exactly that release (the
 * installer ignores its channel then); without one it takes the newest
 * stable release, which may not match the control plane.
 */
export function installCommand(tag: string | null): string {
  const pin = tag ? ` -s -- ${tag}` : ''
  // Without pipefail a failed download feeds bash nothing, which exits 0,
  // and the next command would run whatever binary was there before.
  return `(set -o pipefail; curl -fsSL ${INSTALLER_URL} | ${AS_ROOT} bash${pin})`
}

/**
 * `value` as one shell word. Join URLs and tokens are plain today, but the
 * URL is an admin setting and the command runs as root, so anything with
 * shell syntax is single-quoted rather than trusted.
 */
export function shellWord(value: string): string {
  if (/^[A-Za-z0-9._~:/@%+=,-]+$/.test(value)) return value
  return `'${value.replace(/'/g, `'\\''`)}'`
}

/** Run a `temps` subcommand as root, from the root install. */
export function asRootTemps(args: string): string {
  return `${AS_ROOT} ${ROOT_TEMPS_BIN} ${args}`
}

export const AGENT_SERVICE_COMMAND = asRootTemps('agent service install')

/**
 * The operating system of the machine being added.
 *
 * A Mac (ADR 090) runs everything as the user, not root: its containers run
 * in that user's Docker app (Docker Desktop, OrbStack, colima, Rancher
 * Desktop), whose socket is in their home, and the agent is a LaunchAgent in
 * their login session. So the Mac command installs into `~/.temps` without
 * sudo and calls that binary by path (the installer's PATH change only
 * applies to shells started afterwards).
 */
export type WorkerOs = 'linux' | 'macos'

/** Where the installer puts Temps for the user running it on a Mac. */
export const USER_TEMPS_BIN = '~/.temps/bin/temps'

/** Install Temps for the current user (a Mac). */
export function userInstallCommand(tag: string | null): string {
  const pin = tag ? ` -s -- ${tag}` : ''
  return `(set -o pipefail; curl -fsSL ${INSTALLER_URL} | bash${pin})`
}

export interface WorkerJoinScriptOptions {
  /** The URL the worker dials, as `temps join` takes it. */
  url: string
  /** The single-use enrollment token. */
  token: string
  /** The control plane's running version, if it could be read. */
  version: string | null | undefined
  /** Extra `temps join` flags, starting with a space (` --ingress delegated`). */
  joinFlags?: string
  /**
   * Move a machine that is already a worker of another control plane. The
   * old cluster loses it, so this is only ever set by an explicit choice.
   */
  force?: boolean
  /** The new machine's OS; Linux unless said otherwise. */
  os?: WorkerOs
}

/**
 * What to paste on the new machine: one command over three lines, chained
 * with `&&` so a failed step stops the rest, and read whole by the shell
 * before it runs, so a sudo password prompt cannot swallow the lines after
 * it. Also says which release it installs.
 */
export function workerJoinScript({
  url,
  token,
  version,
  joinFlags = '',
  force = false,
  os = 'linux',
}: WorkerJoinScriptOptions): { command: string; pinnedTo: string | null } {
  const tag = releaseTag(version)
  const join = `join ${shellWord(url)} ${shellWord(token)}${joinFlags}${force ? ' --force' : ''}`
  if (os === 'macos') {
    return {
      pinnedTo: tag,
      command: [
        userInstallCommand(tag),
        `${USER_TEMPS_BIN} ${join}`,
        `${USER_TEMPS_BIN} agent service install`,
      ].join(' && \\\n'),
    }
  }
  return {
    pinnedTo: tag,
    command: [
      installCommand(tag),
      asRootTemps(join),
      AGENT_SERVICE_COMMAND,
    ].join(' && \\\n'),
  }
}
