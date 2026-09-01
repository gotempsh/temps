// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { readFile, writeFile, mkdir } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { join, dirname } from 'node:path'

export interface TempsProjectConfig {
  projectSlug: string
  environmentName?: string
  instanceUrl?: string
}

const CONFIG_DIR = '.temps'
const CONFIG_FILE = 'config.json'
const GITIGNORE_FILE = '.gitignore'

/**
 * Walk upward from cwd to find a .temps/config.json file (like .git discovery)
 */
export function findProjectConfigDir(startDir?: string): string | null {
  let dir = startDir ?? process.cwd()

  while (true) {
    const configPath = join(dir, CONFIG_DIR, CONFIG_FILE)
    if (existsSync(configPath)) {
      return join(dir, CONFIG_DIR)
    }

    const parent = dirname(dir)
    if (parent === dir) break // reached filesystem root
    dir = parent
  }

  return null
}

/**
 * Get the path where .temps/config.json would be created (in cwd)
 */
export function getProjectConfigPath(): string {
  return join(process.cwd(), CONFIG_DIR, CONFIG_FILE)
}

/**
 * Read the local project config, walking upward to find it
 */
export async function readProjectConfig(): Promise<TempsProjectConfig | null> {
  const configDir = findProjectConfigDir()
  if (!configDir) return null

  try {
    const configPath = join(configDir, CONFIG_FILE)
    const content = await readFile(configPath, 'utf-8')
    return JSON.parse(content) as TempsProjectConfig
  } catch {
    return null
  }
}

/**
 * Write the local project config in the current directory
 */
export async function writeProjectConfig(config: TempsProjectConfig): Promise<string> {
  const configDir = join(process.cwd(), CONFIG_DIR)

  // Ensure .temps directory exists
  await mkdir(configDir, { recursive: true })

  // Write config.json
  const configPath = join(configDir, CONFIG_FILE)
  await writeFile(configPath, JSON.stringify(config, null, 2) + '\n', 'utf-8')

  // Write .gitignore to prevent accidental commits
  const gitignorePath = join(configDir, GITIGNORE_FILE)
  if (!existsSync(gitignorePath)) {
    await writeFile(gitignorePath, '*\n', 'utf-8')
  }

  return configPath
}

/**
 * Check if a local project config exists in the current directory tree
 */
export function hasProjectConfig(): boolean {
  return findProjectConfigDir() !== null
}
