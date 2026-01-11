/**
 * Check if the current environment is an interactive TTY
 */
export function isTTY(): boolean {
  return Boolean(process.stdin.isTTY && process.stdout.isTTY)
}

/**
 * Determine if we should run in interactive mode based on:
 * 1. Explicit flag (--interactive or --no-interactive)
 * 2. TTY detection (fallback)
 *
 * @param explicitInteractive - Value from --interactive/--no-interactive flag (undefined if not set)
 * @returns true if should run interactively
 */
export function shouldBeInteractive(explicitInteractive?: boolean): boolean {
  // Explicit flag always wins
  if (explicitInteractive !== undefined) {
    return explicitInteractive
  }

  // Fall back to TTY detection
  return isTTY()
}

/**
 * Check if running in CI environment
 */
export function isCI(): boolean {
  return Boolean(
    process.env.CI ||
    process.env.CONTINUOUS_INTEGRATION ||
    process.env.GITHUB_ACTIONS ||
    process.env.GITLAB_CI ||
    process.env.CIRCLECI ||
    process.env.JENKINS_URL ||
    process.env.BUILDKITE
  )
}
