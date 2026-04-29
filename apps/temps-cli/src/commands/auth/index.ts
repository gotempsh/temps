import type { Command } from 'commander'
import { login } from './login.js'
import { logout } from './logout.js'
import { whoami } from './whoami.js'

export function registerAuthCommands(program: Command): void {
  program
    .command('login [url]')
    .description('Authenticate with a Temps server using email + password (or other methods)')
    .option('-k, --api-key <key>', 'Paste an API key instead of prompting for password')
    .option('--email [email]', 'Login with email + password (default flow)')
    .option('--magic [email]', 'Login via magic link (email-based)')
    .option('--context <name>', 'Save the credentials under this context name (defaults to URL host)')
    .option('--mfa <code>', 'Six-digit TOTP code (use in scripts to skip the interactive prompt)')
    .action(async (url: string | undefined, opts: Record<string, unknown>) => {
      // Forward the positional `url` as if it were `--url`. Commander
      // doesn't surface positional args via opts.
      await login({
        ...opts,
        url: typeof url === 'string' && url.length > 0 ? url : (opts.url as string | undefined),
      })
    })

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
