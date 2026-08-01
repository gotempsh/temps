import { useAuth } from '@/contexts/AuthContext'

/**
 * Roles that carry the `audit:read` permission (see
 * `crates/temps-auth/src/permissions.rs`). The audit log is a platform-wide,
 * security-sensitive view (it records every user's and admin's actions, IP
 * addresses and emails), so only the administration roles may read it.
 */
const AUDIT_LOG_ROLES = ['admin', 'platform_admin']

/**
 * Whether the current user may view audit logs. Used to hide audit-log UI
 * entry points so unauthorized roles never issue requests that would 403.
 */
export function useCanViewAuditLogs(): boolean {
  const { user } = useAuth()
  return !!user && AUDIT_LOG_ROLES.includes(user.role)
}
