// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The file on the server that holds the public host key of `algorithm` (as
 * the SSH handshake names it, e.g. `ssh-ed25519`, `ecdsa-sha2-nistp256`,
 * `rsa-sha2-512`), or null when it is not one of the standard key types.
 */
export function hostKeyFileForAlgorithm(algorithm: string): string | null {
  const name = algorithm.trim().toLowerCase()
  if (name === 'ssh-ed25519') return '/etc/ssh/ssh_host_ed25519_key.pub'
  if (name.startsWith('ecdsa-')) return '/etc/ssh/ssh_host_ecdsa_key.pub'
  if (name === 'ssh-rsa' || name.startsWith('rsa-')) {
    return '/etc/ssh/ssh_host_rsa_key.pub'
  }
  return null
}

/** Every host key's fingerprint, for key types without a known file. */
export const ALL_HOST_KEYS_COMMAND =
  'for f in /etc/ssh/ssh_host_*_key.pub; do ssh-keygen -lf "$f"; done'

/**
 * The command that prints, on the server itself, the fingerprint to compare
 * with the one presented over the network.
 */
export function hostKeyCompareCommand(algorithm: string): string {
  const file = hostKeyFileForAlgorithm(algorithm)
  return file ? `ssh-keygen -lf ${file}` : ALL_HOST_KEYS_COMMAND
}

/**
 * The SSH port typed into the enroll form, or why it cannot be used. The form
 * used to read anything unparseable as 22 and pass 70000 through, so a typo
 * probed a different port than the one shown.
 */
export function parseSshPort(
  value: string
): { port: number; error: null } | { port: null; error: string } {
  const trimmed = value.trim()
  if (trimmed === '') return { port: null, error: 'Enter a port (usually 22)' }
  if (!/^\d+$/.test(trimmed)) {
    return { port: null, error: 'The port must be a whole number' }
  }
  const port = Number(trimmed)
  if (port < 1 || port > 65535) {
    return { port: null, error: 'The port must be between 1 and 65535' }
  }
  return { port, error: null }
}
