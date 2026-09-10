// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Shared .env file parsing utility.
 * Handles comments, empty lines, quoted values, and escape sequences.
 */
import { existsSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'

/**
 * Parse a .env file content string into a key-value record.
 * Supports: comments (#), empty lines, KEY=VALUE, single/double quoted values,
 * escape sequences (\n, \", \').
 */
export function parseEnvFile(content: string): Record<string, string> {
  const variables: Record<string, string> = {}

  for (const line of content.split('\n')) {
    const trimmed = line.trim()

    // Skip empty lines and comments
    if (!trimmed || trimmed.startsWith('#')) continue

    // Parse KEY=VALUE
    const match = trimmed.match(/^([^=]+)=(.*)$/)
    if (!match) continue

    const [, key, rawValue] = match
    if (!key || rawValue === undefined) continue

    let value = rawValue.trim()

    // Handle quoted values
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value
        .slice(1, -1)
        .replace(/\\n/g, '\n')
        .replace(/\\"/g, '"')
        .replace(/\\'/g, "'")
    }

    variables[key.trim()] = value
  }

  return variables
}

/**
 * Read and parse a .env file from disk.
 * Returns null if the file doesn't exist.
 */
export function readEnvFile(filePath: string): Record<string, string> | null {
  const resolved = resolve(filePath)
  if (!existsSync(resolved)) {
    return null
  }
  const content = readFileSync(resolved, 'utf-8')
  return parseEnvFile(content)
}

/**
 * Look for common .env file names in a directory.
 * Returns the paths of files that exist, ordered by priority.
 */
export function findEnvFiles(dir?: string): string[] {
  const cwd = dir ?? process.cwd()
  const candidates = ['.env', '.env.local', '.env.development', '.env.example']
  const found: string[] = []

  for (const name of candidates) {
    const fullPath = resolve(cwd, name)
    if (existsSync(fullPath)) {
      found.push(name)
    }
  }

  return found
}
