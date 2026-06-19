#!/usr/bin/env bun
import { Command } from 'commander'
import { getProjects } from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from './lib/client.ts'
import { loadCommand } from './commands/load.ts'
import { scenarioCommand } from './commands/scenario.ts'
import { examplesCommand } from './commands/examples.ts'

const program = new Command()

program
  .name('temps-e2e')
  .description('End-to-end + load testing CLI for a live Temps instance')
  .version('0.1.0')

// Global connection options (also read from TEMPS_URL / TEMPS_API_KEY).
program
  .option('--url <url>', 'Temps instance base URL (default: $TEMPS_URL or http://localhost:8080)')
  .option('--api-key <key>', 'API key (default: $TEMPS_API_KEY)')

function connection(): { url?: string; apiKey?: string } {
  const o = program.opts<{ url?: string; apiKey?: string }>()
  return { url: o.url, apiKey: o.apiKey }
}

program
  .command('ping')
  .description('Verify connectivity + auth against the instance')
  .option('--json', 'machine-readable output')
  .action(async (opts: { json?: boolean }) => {
    const client = makeClient(resolveConfig(connection()))
    const data = unwrap(
      await getProjects({ client, query: { page: 1, per_page: 1 } }),
      'getProjects',
    ) as { total?: number }
    if (opts.json) {
      process.stdout.write(JSON.stringify({ ok: true, projects: data.total ?? 0 }) + '\n')
    } else {
      process.stdout.write(`OK — connected. ${data.total ?? 0} project(s) visible.\n`)
    }
  })

program
  .command('load')
  .description('Generate HTTP load against a URL (no Temps deploy required)')
  .argument('<url>', 'target URL to hammer')
  .option('-n, --requests <n>', 'total requests to send', '1000')
  .option('-c, --concurrency <n>', 'max in-flight requests', '50')
  .option('-d, --duration <dur>', 'run for a duration instead of a fixed count (e.g. 60s, 2m)')
  .option('-m, --method <method>', 'HTTP method', 'GET')
  .option('-H, --header <header...>', 'request header "Key: Value" (repeatable)')
  .option('--timeout <ms>', 'per-request timeout in ms')
  .option('--json', 'machine-readable output')
  .action(loadCommand)

program
  .command('scenario')
  .description('Full lifecycle: project -> deploy image -> wait healthy -> load -> verify -> teardown')
  .option('--image <ref>', 'public Docker image to deploy', 'ghcr.io/temps-sh/e2e-hello:latest')
  .option('--port <port>', 'container port the image listens on', '80')
  .option('-n, --requests <n>', 'load requests after deploy', '2000')
  .option('-c, --concurrency <n>', 'load concurrency', '50')
  .option('--with-db', 'also provision a postgres service')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await scenarioCommand({ ...opts, connection: connection() })
  })

program
  .command('examples')
  .description('Build the repo example projects (Go, Python, Node, …) and run the full deploy/verify lifecycle for each')
  .option('--only <key...>', 'run only these example keys (repeatable); default = fast subset')
  .option('--all', 'run every registered example (includes heavy Rust/Vite builds)')
  .option('--registry <host>', 'registry to push built images to so Temps can pull them (e.g. localhost:5111); or $TEMPS_E2E_REGISTRY')
  .option('--list', 'list available examples and exit')
  .option('-n, --requests <n>', 'load requests per example', '500')
  .option('-c, --concurrency <n>', 'load concurrency', '25')
  .option('--with-db', 'also provision a postgres service per example')
  .option('--keep', 'do not tear down created resources')
  .option('--deploy-timeout <ms>', 'max wait for each deploy to go healthy', '300000')
  .option('--json', 'machine-readable output')
  .action(async (opts) => {
    await examplesCommand({ ...opts, connection: connection() })
  })

program.parseAsync().catch((err: unknown) => {
  process.stderr.write(`\nerror: ${(err as Error).message}\n`)
  process.exit(1)
})
