import { describe, expect, test } from 'bun:test'
import type { PermissionInfo, RoleInfo } from '@/api/client'
import {
  categoryLabel,
  effectivePlatformRole,
  groupRolePermissions,
  groupRolesPermissions,
  permissionAction,
} from './role-permissions'

const permissions: PermissionInfo[] = [
  { name: 'projects:read', category: 'PROJECTS', description: 'View projects' },
  { name: 'users:manage', category: 'USERS', description: 'Manage users' },
  {
    name: 'projects:create',
    category: 'PROJECTS',
    description: 'Create projects',
  },
]

const role: RoleInfo = {
  name: 'admin',
  description: 'Administrator',
  permissions: [
    'users:manage',
    'projects:read',
    'projects:create',
    'projects:read',
  ],
}

describe('role permissions', () => {
  test('does not infer all-project access without a recognized role', () => {
    expect(effectivePlatformRole([])).toBeNull()
    expect(effectivePlatformRole(['unknown'])).toBeNull()
    expect(effectivePlatformRole(['user'])).toBe('user')
    expect(effectivePlatformRole(['user', 'admin'])).toBe('admin')
  })

  test('groups every granted permission without dropping identifiers', () => {
    const groups = groupRolePermissions(role, permissions)
    expect(groups.map((group) => group.category)).toEqual(['projects', 'users'])
    expect(
      groups.flatMap((group) => group.permissions.map((item) => item.name))
    ).toEqual(['projects:read', 'projects:create', 'users:manage'])
  })

  test('formats categories and actions for display', () => {
    expect(categoryLabel('api_keys')).toBe('Api Keys')
    expect(permissionAction('notification_providers:create')).toBe('create')
  })

  test('shows the de-duplicated effective union for multiple roles', () => {
    const userRole: RoleInfo = {
      name: 'user',
      description: 'User',
      permissions: ['projects:read'],
    }
    const groups = groupRolesPermissions([userRole, role], permissions)

    expect(
      groups.flatMap((group) => group.permissions.map((item) => item.name))
    ).toEqual(['projects:read', 'projects:create', 'users:manage'])
  })
})
