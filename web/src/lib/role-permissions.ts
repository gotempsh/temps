import type { PermissionInfo, RoleInfo } from '@/api/client'

export function effectivePlatformRole(
  roleNames: string[]
): 'admin' | 'user' | null {
  if (roleNames.includes('admin')) return 'admin'
  if (roleNames.includes('user')) return 'user'
  return null
}

export type PermissionGroup = {
  category: string
  permissions: PermissionInfo[]
}

export function groupRolePermissions(
  role: RoleInfo | undefined,
  availablePermissions: PermissionInfo[]
): PermissionGroup[] {
  return groupRolesPermissions(role ? [role] : [], availablePermissions)
}

export function groupRolesPermissions(
  roles: RoleInfo[],
  availablePermissions: PermissionInfo[]
): PermissionGroup[] {
  if (roles.length === 0) return []

  const permissionByName = new Map(
    availablePermissions.map((permission) => [permission.name, permission])
  )
  const groups = new Map<string, PermissionInfo[]>()
  const seen = new Set<string>()

  for (const role of roles) {
    for (const name of role.permissions) {
      if (seen.has(name)) continue
      seen.add(name)
      const permission = permissionByName.get(name) ?? {
        name,
        category: name.split(':')[0] || 'OTHER',
        description: 'Permission for this resource',
      }
      const category = permission.category.toLowerCase()
      const existing = groups.get(category) ?? []
      existing.push(permission)
      groups.set(category, existing)
    }
  }

  return Array.from(groups, ([category, permissions]) => ({
    category,
    permissions,
  })).sort((left, right) => left.category.localeCompare(right.category))
}

export function permissionAction(permissionName: string): string {
  const segments = permissionName.split(':')
  const action = segments[segments.length - 1] ?? permissionName
  return action.replace(/_/g, ' ')
}

export function categoryLabel(category: string): string {
  return category
    .split(/[_-]/)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}
