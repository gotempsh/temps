/**
 * CLI contexts — one set of credentials per Temps server the user has
 * logged into. Lets the same workstation stay authenticated against
 * multiple Temps deployments simultaneously (prod + local + a teammate's
 * dev install) and switch with a single command.
 *
 * NOT to be confused with `config/instances.ts`, which manages Temps
 * Cloud VPS instances (provisioned servers).
 *
 * Storage:
 *   ~/.temps/.contexts.json   — mode 0600, JSON array of Context records.
 *
 * Each context carries:
 *   - name           friendly key the user types (e.g. "prod", "local")
 *   - url            base URL of the Temps server (no /api suffix; we
 *                    add it where needed via normalizeApiUrl)
 *   - apiKey         the bearer token (minted by the device-flow approval,
 *                    or pasted from the dashboard via `--api-key`)
 *   - email          the logged-in user's email (informational)
 *   - keyPrefix      first 8 chars of the API key — lets the user audit
 *                    the key in the web UI without revealing the secret
 *   - expiresAt      ISO 8601 timestamp when the API key expires
 *   - isActive       only one context is the active one at a time
 */

import { readFile, writeFile, mkdir, unlink } from 'node:fs/promises'
import { existsSync, readFileSync } from 'node:fs'
import { dirname } from 'node:path'

export interface CliContext {
  name: string
  url: string
  apiKey: string
  email: string
  keyPrefix?: string
  expiresAt?: string
  isActive?: boolean
}

function getContextsPath(): string {
  const home = process.env.HOME || process.env.USERPROFILE || '~'
  return `${home}/.temps/.contexts.json`
}

async function loadContexts(): Promise<CliContext[]> {
  const path = getContextsPath()
  try {
    if (existsSync(path)) {
      const content = await readFile(path, 'utf-8')
      const parsed = JSON.parse(content) as unknown
      if (Array.isArray(parsed)) return parsed as CliContext[]
    }
  } catch {
    // Missing or malformed — treat as empty.
  }
  return []
}

async function saveContexts(contexts: CliContext[]): Promise<void> {
  const path = getContextsPath()
  await mkdir(dirname(path), { recursive: true })
  // Strip undefined fields so the file stays compact and stable.
  const normalized = contexts.map((c) => {
    const out: CliContext = {
      name: c.name,
      url: c.url,
      apiKey: c.apiKey,
      email: c.email,
    }
    if (c.keyPrefix !== undefined) out.keyPrefix = c.keyPrefix
    if (c.expiresAt !== undefined) out.expiresAt = c.expiresAt
    if (c.isActive !== undefined) out.isActive = c.isActive
    return out
  })
  await writeFile(path, JSON.stringify(normalized, null, 2) + '\n', { mode: 0o600 })
}

/** All contexts. Empty array if no file. */
export async function listContexts(): Promise<CliContext[]> {
  return loadContexts()
}

/** Get a context by name, or null. */
export async function getContext(name: string): Promise<CliContext | null> {
  const contexts = await loadContexts()
  return contexts.find((c) => c.name === name) ?? null
}

/** Get the active context (or the only one when nothing is marked active). */
export async function getActiveContext(): Promise<CliContext | null> {
  const contexts = await loadContexts()
  if (contexts.length === 0) return null
  return contexts.find((c) => c.isActive) ?? contexts[0] ?? null
}

/**
 * Add a new context, or replace one with the same name.
 * The new/updated context becomes active. Pass `makeActive: false` to
 * keep the existing active context.
 */
export async function upsertContext(
  context: CliContext,
  options: { makeActive?: boolean } = {},
): Promise<void> {
  const makeActive = options.makeActive ?? true
  const contexts = await loadContexts()
  const idx = contexts.findIndex((c) => c.name === context.name)
  if (idx >= 0) {
    contexts[idx] = { ...context }
  } else {
    contexts.push({ ...context })
  }
  if (makeActive || contexts.length === 1) {
    for (const c of contexts) {
      c.isActive = c.name === context.name
    }
  }
  await saveContexts(contexts)
}

/** Remove a context. Returns true if it existed. */
export async function removeContext(name: string): Promise<boolean> {
  const contexts = await loadContexts()
  const filtered = contexts.filter((c) => c.name !== name)
  if (filtered.length === contexts.length) return false
  // If we removed the active one, make the first remaining context active
  // so subsequent commands aren't suddenly "not logged in".
  if (!filtered.some((c) => c.isActive) && filtered.length > 0) {
    filtered[0]!.isActive = true
  }
  if (filtered.length === 0) {
    // Don't leave an empty 0600 husk on disk.
    const path = getContextsPath()
    if (existsSync(path)) {
      try {
        await unlink(path)
      } catch {
        // Best effort.
      }
    }
    return true
  }
  await saveContexts(filtered)
  return true
}

/**
 * Make `name` the active context. Returns false if the context doesn't
 * exist.
 */
export async function setActiveContext(name: string): Promise<boolean> {
  const contexts = await loadContexts()
  if (!contexts.some((c) => c.name === name)) return false
  for (const c of contexts) {
    c.isActive = c.name === name
  }
  await saveContexts(contexts)
  return true
}

/**
 * Derive a default context name from a URL. Uses the host portion so
 * `https://temps.example.com` becomes `temps.example.com`.
 */
export function defaultContextName(url: string): string {
  try {
    const parsed = new URL(url)
    return parsed.host || 'default'
  } catch {
    return 'default'
  }
}

/** Path to the contexts file (for display in UI / errors). */
export function contextsPath(): string {
  return getContextsPath()
}

/**
 * Synchronous reader for the active context. Used by `config/store.ts` to
 * resolve `apiUrl` / `apiKey` for callers that haven't migrated to async
 * lookups (i.e. all existing commands). Returns null if no contexts file
 * exists or if it can't be parsed — callers fall back to legacy storage.
 */
export function getActiveContextSync(): CliContext | null {
  try {
    const path = getContextsPath()
    if (!existsSync(path)) return null
    const content = readFileSync(path, 'utf-8')
    const parsed = JSON.parse(content) as unknown
    if (!Array.isArray(parsed) || parsed.length === 0) return null
    const list = parsed as CliContext[]
    return list.find((c) => c.isActive) ?? list[0] ?? null
  } catch {
    return null
  }
}
