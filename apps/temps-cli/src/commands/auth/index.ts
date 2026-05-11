import type { Command } from 'commander'
import { login } from './login.js'
import { logout } from './logout.js'
import { whoami } from './whoami.js'

export function registerAuthCommands(program: Command): void {
  program
    .command('login [url]')
    .description('Authenticate with a Temps server. Opens the browser for interactive logins; use --api-key for headless / CI.')
    .option('-k, --api-key <key>', 'Use a pre-minted API key (Settings → API Keys) instead of opening the browser. Required for headless / CI.')
    .option('--context <name>', 'Save the credentials under this context name (defaults to URL host)')
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
