// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps users` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  changePasswordSelf,
  disableMfa,
  setupMfa,
  updateSelf,
  updateUser,
  verifyAndEnableMfa,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerUsersEndpoints(program: Command): void {
  const group = topGroup(program, 'users', "users")

  endpoint(group, updateSelf, {
    name: "update-self",
    description: "Update current user's information",
    body: {
      email: 'string',
      name: 'string',
    },
  })

  endpoint(group, disableMfa, {
    name: "disable-mfa",
    description: "Disable mfa",
    body: {
      code: 'string!',
    },
    confirm: "This will disable mfa.",
  })

  endpoint(group, setupMfa, {
    name: "setup-mfa",
    description: "Setup mfa",
    body: {
      current_password: { kind: 'secret', help: "Required to enroll MFA on an account that has a password set" },
    },
  })

  endpoint(group, verifyAndEnableMfa, {
    name: "verify-and-enable-mfa",
    description: "Verify and enable mfa",
    body: {
      code: 'string!',
    },
  })

  endpoint(group, changePasswordSelf, {
    name: "change-password-self",
    description: "Change password self",
    body: {
      current_password: 'secret!',
      mfa_code: { kind: 'string', help: "TOTP code (or recovery code)" },
      new_password: 'secret!',
      revoke_other_sessions: { kind: 'bool', help: "When true, every session OTHER than the one making this request is revoked" },
    },
  })

  endpoint(group, updateUser, {
    name: "update",
    description: "Update user information (admin only)",
    path: { user_id: 'int' },
    body: {
      email: 'string',
      name: { kind: 'string', help: "Deprecated: the old name of 'username', still accepted" },
      username: { kind: 'string', help: "The new username, as in 'CreateUserRequest'" },
    },
  })
}
