// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Roles that administer the whole installation (see `Role::Admin` and
 * `Role::PlatformAdmin` in `crates/temps-auth/src/permissions.rs`).
 *
 * Only these roles hold `settings:read`, `dns_providers:read` and
 * `users:write`, and only they pass the server's instance-admin gate on
 * global resources such as backup alerts. Every other role (`user`,
 * `reader`, ...) is refused with a 403, so the console must not issue those
 * requests for them at all -- several are polled from the app shell, and a
 * refused poll repeats for as long as a tab stays open.
 */
const INSTANCE_ADMIN_ROLES: ReadonlySet<string> = new Set([
  'admin',
  'platform_admin',
])

/** Whether a signed-in user's effective role administers the instance. */
export function isInstanceAdmin(role: string | null | undefined): boolean {
  return typeof role === 'string' && INSTANCE_ADMIN_ROLES.has(role)
}
