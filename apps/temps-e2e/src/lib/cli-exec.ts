/**
 * Spawns the REAL @temps-sdk/cli binary as a subprocess against a live
 * instance, so cli-scenario.ts proves what a unit test never can: that argv
 * parsing, Commander's command wiring, the action handler, and stdout
 * formatting all actually work together end to end -- the same path an AI
 * agent takes running `bunx @temps-sdk/cli ...`.
 *
 * Runs the local checkout's source directly (`bun run src/index.ts`) rather
 * than `bunx @temps-sdk/cli`, so this always exercises the code under test in
 * this repo, not whatever version is published/linked. Same source Commander
 * registers either way, so command wiring is genuinely identical.
 */
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

const CLI_ENTRY = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../temps-cli/src/index.ts',
)

export interface CliResult {
  stdout: string
  stderr: string
  exitCode: number
}

export interface CliConfig {
  url: string
  apiKey: string
}

/**
 * Runs one CLI invocation to completion and returns its full output.
 * Always prepends `--no-color` so assertions never have to strip ANSI codes.
 * Auth is passed via TEMPS_API_URL / TEMPS_API_KEY (see api-client.ts's
 * resolution order) rather than flags -- there is no global --url/--api-key
 * on this CLI, only on `temps login`.
 */
export function runCli(
  args: string[],
  cfg: CliConfig,
  opts: { timeoutMs?: number; input?: string } = {},
): Promise<CliResult> {
  const timeoutMs = opts.timeoutMs ?? 30_000
  return new Promise((resolve, reject) => {
    const child = spawn('bun', ['run', CLI_ENTRY, '--no-color', ...args], {
      env: {
        ...process.env,
        TEMPS_API_URL: cfg.url,
        TEMPS_API_KEY: cfg.apiKey,
        // A spawned subprocess has no TTY -- make sure prompt-driven paths
        // fail loudly instead of hanging forever waiting for input that will
        // never come.
        CI: '1',
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    })

    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (c: Buffer) => (stdout += c.toString('utf8')))
    child.stderr.on('data', (c: Buffer) => (stderr += c.toString('utf8')))

    const timer = setTimeout(() => {
      child.kill('SIGKILL')
      reject(
        new Error(
          `temps ${args.join(' ')} timed out after ${timeoutMs}ms\nstdout: ${stdout}\nstderr: ${stderr}`,
        ),
      )
    }, timeoutMs)

    if (opts.input !== undefined) {
      child.stdin.write(opts.input)
    }
    child.stdin.end()

    child.on('error', (err) => {
      clearTimeout(timer)
      reject(err)
    })
    child.on('close', (code) => {
      clearTimeout(timer)
      resolve({ stdout, stderr, exitCode: code ?? -1 })
    })
  })
}

/**
 * Runs a CLI command and asserts it succeeded (exit 0), throwing with the
 * full stdout/stderr on failure -- every scenario step should fail loudly
 * with the real CLI output, not a generic "expected 0" message.
 */
export async function runCliOk(
  args: string[],
  cfg: CliConfig,
  opts?: { timeoutMs?: number; input?: string },
): Promise<CliResult> {
  const result = await runCli(args, cfg, opts)
  if (result.exitCode !== 0) {
    throw new Error(
      `temps ${args.join(' ')} exited ${result.exitCode}\nstdout: ${result.stdout}\nstderr: ${result.stderr}`,
    )
  }
  return result
}

/** Runs a `--json`-flagged command and parses its stdout as JSON. */
export async function runCliJson<T = unknown>(
  args: string[],
  cfg: CliConfig,
  opts?: { timeoutMs?: number },
): Promise<T> {
  const result = await runCliOk([...args, '--json'], cfg, opts)
  try {
    return JSON.parse(result.stdout) as T
  } catch (e) {
    throw new Error(
      `temps ${args.join(' ')} --json produced unparsable output: ${(e as Error).message}\nstdout: ${result.stdout}`,
    )
  }
}
