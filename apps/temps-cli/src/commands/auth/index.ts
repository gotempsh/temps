// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { addCredentialSources } from '../../lib/value-source.js'
import { login } from './login.js'
import { logout } from './logout.js'
import { whoami } from './whoami.js'

/**
 * Forward the positional `url` argument as if it were `--url`. Commander
 * doesn't surface positional args via `opts`, and an empty positional
 * (`temps login ""`) must not shadow a real `--url` value.
 */
export function resolveLoginUrl(
  positionalUrl: string | undefined,
  opts: Record<string, unknown>,
): string | undefined {
  return typeof positionalUrl === 'string' && positionalUrl.length > 0
    ? positionalUrl
    : (opts.url as string | undefined)
}

export function registerAuthCommands(program: Command): void {
  const loginCommand = program
    .command('login [url]')
    .description('Authenticate with a Temps server. Opens the browser for interactive logins; use --no-wait then --resume when a person approves but the caller cannot keep a command open, and --api-key for headless / CI.')
    .option('-k, --api-key <key>', 'Use a pre-minted API key (Settings → API Keys) instead of opening the browser. Required for headless / CI.')
    .option('--context <name>', 'Save the credentials under this context name (defaults to URL host)')
    .option('--no-wait', 'Print the browser approval link and exit instead of waiting for it. For agents and scripts whose shell returns output only when a command exits; finish with --resume.')
    .option('--resume', 'Finish a sign-in started with --no-wait: wait for the browser approval, then save the credentials. Exits with code 2 if it is not approved yet; run it again.')
    .option('--wait-secs <seconds>', 'With --resume: how long to wait for the approval before exiting with code 2 (default: 90)')
    .option('--debug', 'Print every request/response (URL, status, headers, raw body) to stderr. Also enabled via TEMPS_DEBUG=1.')
    .action(async (url: string | undefined, opts: Record<string, unknown>) => {
      await login({
        ...opts,
        url: resolveLoginUrl(url, opts),
      })
    })
  addCredentialSources(loginCommand, [{ flag: 'api-key', what: 'the API key' }])

  program
    .command('logout')
    .description('Revoke the active context\'s API key on the server and forget it locally')
    .option('--context <name>', 'Log out of a specific context (defaults to active)')
    .option('--local-only', 'Skip server-side revocation; only clear local credentials')
    .action(logout)

  program
    .command('whoami')
    .description('Display current authenticated user and active context')
    .option('--json', 'Output as JSON')
    .action(whoami)
}
