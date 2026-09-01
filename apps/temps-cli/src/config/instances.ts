// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { readFile, writeFile, mkdir } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { dirname } from 'node:path'

export interface TempsInstance {
  name: string
  url: string
  apiKey?: string
  email?: string
  isDefault?: boolean
}

/**
 * Get the path to the instances file
 */
function getInstancesPath(): string {
  const home = process.env.HOME || process.env.USERPROFILE || '~'
  return `${home}/.temps/.instances.json`
}

/**
 * Load instances from disk
 */
async function loadInstances(): Promise<TempsInstance[]> {
  const path = getInstancesPath()
  try {
    if (existsSync(path)) {
      const content = await readFile(path, 'utf-8')
      return JSON.parse(content) as TempsInstance[]
    }
  } catch {
    // File doesn't exist or is malformed
  }
  return []
}

/**
 * Save instances to disk
 */
async function saveInstances(instances: TempsInstance[]): Promise<void> {
  const path = getInstancesPath()
  const dir = dirname(path)
  await mkdir(dir, { recursive: true })
  await writeFile(path, JSON.stringify(instances, null, 2) + '\n', { mode: 0o600 })
}

/**
 * List all configured instances
 */
export async function listInstances(): Promise<TempsInstance[]> {
  return loadInstances()
}

/**
 * Add or update an instance
 */
export async function addInstance(instance: TempsInstance): Promise<void> {
  const instances = await loadInstances()
  const existing = instances.findIndex(i => i.name === instance.name)

  if (existing >= 0) {
    instances[existing] = instance
  } else {
    instances.push(instance)
  }

  // If this is the first instance or marked as default, ensure only one default
  if (instance.isDefault || instances.length === 1) {
    for (const inst of instances) {
      inst.isDefault = inst.name === instance.name
    }
  }

  await saveInstances(instances)
}

/**
 * Remove an instance by name
 */
export async function removeInstance(name: string): Promise<boolean> {
  const instances = await loadInstances()
  const filtered = instances.filter(i => i.name !== name)

  if (filtered.length === instances.length) {
    return false // nothing removed
  }

  // If we removed the default, make the first remaining instance default
  if (!filtered.some(i => i.isDefault) && filtered.length > 0) {
    filtered[0]!.isDefault = true
  }

  await saveInstances(filtered)
  return true
}

/**
 * Set an instance as the default
 */
export async function setDefaultInstance(name: string): Promise<boolean> {
  const instances = await loadInstances()
  const target = instances.find(i => i.name === name)

  if (!target) return false

  for (const inst of instances) {
    inst.isDefault = inst.name === name
  }

  await saveInstances(instances)
  return true
}

/**
 * Get the default instance
 */
export async function getDefaultInstance(): Promise<TempsInstance | null> {
  const instances = await loadInstances()
  return instances.find(i => i.isDefault) ?? instances[0] ?? null
}

/**
 * Get an instance by name
 */
export async function getInstance(name: string): Promise<TempsInstance | null> {
  const instances = await loadInstances()
  return instances.find(i => i.name === name) ?? null
}
