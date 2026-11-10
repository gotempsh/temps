// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A key can never carry more permissions than its creator holds: the server
 * refuses the request with `PERMISSION_CEILING_EXCEEDED` when a role or an
 * explicit permission exceeds the signed-in user's own (see
 * `enforce_permission_ceiling_for_role` in `crates/temps-auth`). The form uses
 * the same rule to avoid offering what will be refused on the last step.
 */

interface CatalogRole {
  name: string
  permissions: string[]
}

/**
 * The permissions the signed-in user holds, read from their own entry in the
 * role catalogue. `undefined` when it cannot be known -- the role is missing
 * from the catalogue, or is `custom` (its permissions are per-user) -- in
 * which case nothing is filtered and the server stays the judge.
 */
export function permissionCeiling(
  roles: readonly CatalogRole[] | undefined,
  userRole: string | null | undefined
): ReadonlySet<string> | undefined {
  if (!roles || !userRole || userRole === 'custom') return undefined
  const own = roles.find((role) => role.name === userRole)
  return own ? new Set(own.permissions) : undefined
}

/** Whether every permission of `role` is within the ceiling. */
export function canGrantRole(
  role: CatalogRole,
  ceiling: ReadonlySet<string> | undefined
): boolean {
  return !ceiling || role.permissions.every((p) => ceiling.has(p))
}

/** Whether a single permission is within the ceiling. */
export function canGrantPermission(
  permission: string,
  ceiling: ReadonlySet<string> | undefined
): boolean {
  return !ceiling || ceiling.has(permission)
}
