// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { mongoshExec } from './mongosh.ts'

test('mongosh authenticates literal credentials, writes data, and rejects wrong passwords', async () => {
  try {
    const docker = Bun.spawnSync(['docker', 'info'], { stdout: 'ignore', stderr: 'ignore' })
    if (docker.exitCode !== 0) {
      console.warn('Skipping MongoDB authentication integration test: Docker unavailable')
      return
    }
  } catch {
    console.warn('Skipping MongoDB authentication integration test: Docker unavailable')
    return
  }

  const name = `temps-mongosh-auth-${crypto.randomUUID()}`
  const username = 'root'
  const password = "-leading 'quoted' $literal password"
  const started = Bun.spawnSync([
    'docker', 'run', '-d', '--name', name,
    '--env', 'MONGO_INITDB_ROOT_USERNAME', '--env', 'MONGO_INITDB_ROOT_PASSWORD',
    'mongo:latest',
  ], {
    env: { ...process.env, MONGO_INITDB_ROOT_USERNAME: username, MONGO_INITDB_ROOT_PASSWORD: password },
    stdout: 'pipe', stderr: 'pipe',
  })
  try {
    expect(started.exitCode, new TextDecoder().decode(started.stderr)).toBe(0)
    let ready = false
    for (let attempt = 0; attempt < 60; attempt++) {
      try {
        await mongoshExec(name, 'auth_test', 'db.adminCommand({ping: 1})')
        ready = true
        break
      } catch {
        await Bun.sleep(1000)
      }
    }
    expect(ready, 'MongoDB did not accept the configured literal credentials').toBe(true)
    await mongoshExec(name, 'auth_test', 'db.probe.insertOne({_id: "verified"})')
    expect(await mongoshExec(name, 'auth_test', 'db.probe.countDocuments({_id: "verified"})')).toBe('1')
    await mongoshExec(name, 'admin', 'db.changeUserPassword("root", "changed-password")')
    await expect(mongoshExec(name, 'auth_test', 'db.probe.countDocuments({})')).rejects.toThrow('Authentication failed')
  } finally {
    Bun.spawnSync(['docker', 'rm', '-f', '-v', name], { stdout: 'ignore', stderr: 'ignore' })
  }
}, 120_000)
