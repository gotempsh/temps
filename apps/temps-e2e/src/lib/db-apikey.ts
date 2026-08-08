/**
 * Mints a bearer API key for an arbitrary user directly from the Rust
 * `temps` binary's own `api-key --database-url=...` subcommand (the same
 * bootstrap path apps/temps-e2e/README.md documents for minting this
 * harness's own root key).
 *
 * Needed specifically for RBAC testing: `POST /auth/login` only sets a
 * session cookie (no bearer token the SDK/CLI's bearer-auth flow can use),
 * and minting a NEW API key while already authenticated via API key is
 * deliberately blocked (`crates/temps-auth/src/apikey_handler.rs`'s
 * SensitiveAction guard, an anti-privilege-escalation boundary -- see
 * cli-scenario.ts). DB-direct minting is the only way to get a second,
 * independent identity's own credential without a real login flow.
 */
export interface DbApiKeyConfig {
  /** Repo checkout root containing crates/temps-cli (Cargo.toml sibling). */
  tempsRoot: string
  /** Postgres URL the `temps` binary can reach directly. */
  databaseUrl: string
}

export interface MintedApiKey {
  apiKey: string
  userId: number
  userEmail: string
}

/** Runs `cargo run --profile fast --bin temps -- api-key ...` and parses the minted key. */
export async function mintDbApiKey(
  cfg: DbApiKeyConfig,
  opts: { name: string; role: 'admin' | 'user'; userEmail: string; timeoutMs?: number },
): Promise<MintedApiKey> {
  const timeoutMs = opts.timeoutMs ?? 120_000
  const cwd = `${cfg.tempsRoot.replace(/\/+$/, '')}/crates/temps-cli`

  const proc = Bun.spawn(
    [
      'cargo',
      'run',
      '--profile',
      'fast',
      '--bin',
      'temps',
      '--package',
      'temps-cli',
      '--',
      'api-key',
      `--database-url=${cfg.databaseUrl}`,
      `--name=${opts.name}`,
      `--role=${opts.role}`,
      `--user-email=${opts.userEmail}`,
      '--output-format=json',
    ],
    { cwd, stdout: 'pipe', stderr: 'pipe' },
  )

  const timer = setTimeout(() => proc.kill(), timeoutMs)
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  clearTimeout(timer)

  if (code !== 0) {
    throw new Error(`temps api-key (${opts.userEmail}) exited ${code}\nstdout: ${stdout}\nstderr: ${stderr}`)
  }

  // `cargo run` prints build progress to stdout/stderr; the JSON payload is
  // the last well-formed JSON object in stdout.
  const jsonStart = stdout.lastIndexOf('{')
  if (jsonStart === -1) {
    throw new Error(`temps api-key (${opts.userEmail}) produced no JSON on stdout:\n${stdout}`)
  }
  let parsed: { api_key: string; user_id: number; user_email: string }
  try {
    parsed = JSON.parse(stdout.slice(jsonStart))
  } catch (e) {
    throw new Error(
      `temps api-key (${opts.userEmail}) produced unparsable JSON: ${(e as Error).message}\nstdout: ${stdout}`,
    )
  }
  return { apiKey: parsed.api_key, userId: parsed.user_id, userEmail: parsed.user_email }
}
