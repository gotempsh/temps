// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Authenticate inside mongosh instead of passing credentials through its CLI
 * parser. This avoids password prompts/flag parsing and keeps credentials out
 * of Docker and mongosh argv. Read the credentials already installed in the
 * target container, just as the provider healthcheck does.
 */
export async function mongoshExec(
  containerName: string,
  database: string,
  script: string,
): Promise<string> {
  const proc = Bun.spawn(
    [
      'docker',
      'exec',
      containerName,
      'mongosh',
      '--quiet',
      '--norc',
      '--eval',
      'db.getSiblingDB("admin").auth(process.env.MONGO_INITDB_ROOT_USERNAME, process.env.MONGO_INITDB_ROOT_PASSWORD); ' + script,
      database,
    ],
    {
      stdout: 'pipe',
      stderr: 'pipe',
    },
  )
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  if (code !== 0) {
    throw new Error(
      `mongosh exec in '${containerName}' exited ${code}.\nstdout: ${stdout.trim()}\nstderr: ${stderr.trim()}`,
    )
  }
  return stdout.trim()
}
