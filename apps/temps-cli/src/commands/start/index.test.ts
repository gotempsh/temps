// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { mkdtempSync } from 'node:fs'
import { mkdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { clearContextDefaultProject, getContext } from '../../config/contexts.js'
import { carriedDefaultProject, savedDefaultElsewhere } from './index.js'

const LOCAL_URL = 'http://127.0.0.1:8700/api'
const saved = { HOME: process.env.HOME, TEMPS_CONTEXT: process.env.TEMPS_CONTEXT }
let home = ''

async function writeContexts(contexts: { name: string; isActive?: boolean; defaultProject?: string }[]): Promise<void> {
  await mkdir(join(home, '.temps'), { recursive: true })
  const records = contexts.map((c) => ({
    ...c,
    url: c.name === 'local' ? LOCAL_URL : `https://${c.name}.example.com`,
    apiKey: `tk_${c.name}`,
    email: `${c.name}@example.com`,
  }))
  await writeFile(join(home, '.temps', '.contexts.json'), JSON.stringify(records))
}

beforeEach(() => {
  home = mkdtempSync(join(tmpdir(), 'temps-start-home-'))
  process.env.HOME = home
  delete process.env.TEMPS_CONTEXT
})

afterEach(async () => {
  for (const [key, value] of Object.entries(saved)) {
    if (value === undefined) delete process.env[key]
    else process.env[key] = value
  }
  await rm(home, { recursive: true, force: true })
})

describe('savedDefaultElsewhere', () => {
  test('reports another saved default by name', async () => {
    await writeContexts([{ name: 'prod', isActive: true }, { name: 'local' }])
    expect(await savedDefaultElsewhere(LOCAL_URL)).toBe('prod')
  })

  test('TEMPS_CONTEXT=local does not hide the saved default it would replace', async () => {
    await writeContexts([{ name: 'prod', isActive: true }, { name: 'local' }])
    process.env.TEMPS_CONTEXT = 'local'
    expect(await savedDefaultElsewhere(LOCAL_URL)).toBe('prod')
  })

  test('local already being the saved default is not "elsewhere", whatever TEMPS_CONTEXT says', async () => {
    await writeContexts([{ name: 'prod' }, { name: 'local', isActive: true }])
    expect(await savedDefaultElsewhere(LOCAL_URL)).toBeNull()
    process.env.TEMPS_CONTEXT = 'prod'
    expect(await savedDefaultElsewhere(LOCAL_URL)).toBeNull()
  })

  test('an unflagged list defaults to its first context', async () => {
    await writeContexts([{ name: 'prod' }, { name: 'local' }])
    expect(await savedDefaultElsewhere(LOCAL_URL)).toBe('prod')
  })
})

describe('the default project across restarts and resets', () => {
  const existing = { name: 'local', url: LOCAL_URL, apiKey: 'tk_local', email: 'admin@localhost', defaultProject: 'shop' }

  test('is carried over only when this server still accepts the saved key', () => {
    expect(carriedDefaultProject(existing, true)).toBe('shop')
    // A new key had to be minted: a reset or another install, where "shop" does not exist.
    expect(carriedDefaultProject(existing, false)).toBeUndefined()
    expect(carriedDefaultProject(null, false)).toBeUndefined()
  })

  test('a reset clears it on the local context and leaves other contexts alone', async () => {
    await writeContexts([
      { name: 'prod', isActive: true, defaultProject: 'site' },
      { name: 'local', defaultProject: 'shop' },
    ])
    expect(await clearContextDefaultProject('local')).toBe(true)
    expect((await getContext('local'))?.defaultProject).toBeUndefined()
    expect((await getContext('local'))?.apiKey).toBe('tk_local')
    const prod = await getContext('prod')
    expect([prod?.defaultProject, prod?.isActive]).toEqual(['site', true])
    // Nothing left to clear, and a context that does not exist, are no-ops.
    expect(await clearContextDefaultProject('local')).toBe(false)
    expect(await clearContextDefaultProject('missing')).toBe(false)
  })
})
