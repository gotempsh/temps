// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerUsersCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('users create --must-change-password', () => {
  test('asks the server to require a new password at first sign-in', async () => {
    const argv = ['users', 'create', '-u', 'ana', '-e', 'ana@example.com', '-r', 'user', '--yes']
    const routes = { 'POST /users': { id: 1 } }
    const run = await runWithApi(registerUsersCommands, [...argv, '--must-change-password'], routes)
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toMatchObject({ username: 'ana', must_change_password: true })
    const plain = await runWithApi(registerUsersCommands, argv, routes)
    expect(plain.bodies[0]?.body).not.toHaveProperty('must_change_password')
  })
})
