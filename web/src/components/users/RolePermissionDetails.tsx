import { getApiKeyPermissionsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
import {
  categoryLabel,
  effectivePlatformRole,
  groupRolesPermissions,
  permissionAction,
} from '@/lib/role-permissions'
import { useQuery } from '@tanstack/react-query'
import { Globe2, ShieldAlert } from 'lucide-react'

const ROLE_SCOPE = {
  user: {
    title: 'All-project access',
    description:
      'Can access every existing project and every project created later. Project access is not limited to selected projects.',
  },
  admin: {
    title: 'Full platform control',
    description:
      'Can manage all projects, users, roles, settings, and system resources, including destructive operations.',
  },
} as const

export function RolePermissionDetails({ roleNames }: { roleNames: string[] }) {
  const { data, isLoading, isError } = useQuery({
    ...getApiKeyPermissionsOptions({}),
  })

  const uniqueRoleNames = Array.from(new Set(roleNames))
  const roles =
    data?.roles.filter((role) => uniqueRoleNames.includes(role.name)) ?? []
  const groups = groupRolesPermissions(roles, data?.permissions ?? [])
  const permissionCount = groups.reduce(
    (total, group) => total + group.permissions.length,
    0
  )
  const effectiveRoleName = effectivePlatformRole(uniqueRoleNames)
  const scope = effectiveRoleName ? ROLE_SCOPE[effectiveRoleName] : undefined

  return (
    <div className="space-y-4">
      {scope && (
        <Alert variant={effectiveRoleName === 'admin' ? 'warning' : 'default'}>
          {effectiveRoleName === 'admin' ? (
            <ShieldAlert className="size-4 shrink-0" />
          ) : (
            <Globe2 className="size-4 shrink-0" />
          )}
          <AlertTitle>{scope.title}</AlertTitle>
          <AlertDescription>
            <p className="text-pretty text-base sm:text-sm">
              {scope.description}
            </p>
          </AlertDescription>
        </Alert>
      )}

      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <div>
          <h3 className="font-medium">Exact permissions</h3>
          <p className="text-pretty text-base text-muted-foreground sm:text-sm">
            {uniqueRoleNames.length > 0
              ? `Effective union of the ${uniqueRoleNames.join(', ')} role${uniqueRoleNames.length === 1 ? '' : 's'}.`
              : 'No platform role is currently assigned.'}
          </p>
        </div>
        {roles.length > 0 && (
          <Badge variant="secondary" className="tabular-nums">
            {permissionCount} permissions
          </Badge>
        )}
      </div>

      {isLoading ? (
        <div className="space-y-3">
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-20 w-full" />
        </div>
      ) : isError || roles.length !== uniqueRoleNames.length ? (
        <Alert variant="destructive">
          <AlertTitle>Permissions unavailable</AlertTitle>
          <AlertDescription>
            <p className="text-base sm:text-sm">
              The exact permission list could not be loaded. No user has been
              created yet.
            </p>
          </AlertDescription>
        </Alert>
      ) : (
        <ScrollArea className="h-[min(32rem,55vh)] rounded-lg border border-border/60">
          <div className="divide-y divide-border/60 p-4">
            {groups.map((group) => (
              <section
                key={group.category}
                className="py-4 first:pt-0 last:pb-0"
              >
                <div className="flex flex-wrap items-baseline justify-between gap-2">
                  <h4 className="font-medium">
                    {categoryLabel(group.category)}
                  </h4>
                  <div className="tabular-nums text-base text-muted-foreground sm:text-sm">
                    {group.permissions.length}
                  </div>
                </div>
                <ul role="list" className="mt-3 grid gap-2 sm:grid-cols-2">
                  {group.permissions.map((permission) => (
                    <li
                      key={permission.name}
                      className="min-w-0 rounded-md bg-muted/50 p-2.5"
                    >
                      <div className="font-medium capitalize">
                        {permissionAction(permission.name)}
                      </div>
                      <div className="truncate font-mono text-base text-muted-foreground sm:text-sm">
                        {permission.name}
                      </div>
                    </li>
                  ))}
                </ul>
              </section>
            ))}
          </div>
        </ScrollArea>
      )}
    </div>
  )
}
