// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  listAuditLogsOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AuditLogItemRow } from '@/components/audit/AuditLogItem'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useCanViewAuditLogs } from '@/hooks/useAuditAccess'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { format, formatDistanceToNow } from 'date-fns'
import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  Calendar,
  Clock,
  LogIn,
  Mail,
  MailCheck,
  MailWarning,
  ScrollText,
  Shield,
} from 'lucide-react'
import { ReactNode, useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'

const ITEMS_PER_PAGE = 20
const STATS_WINDOW_MS = 30 * 24 * 60 * 60 * 1000
const STATS_BATCH_SIZE = 200

export function UserDetail() {
  const { userId } = useParams<{ userId: string }>()
  const parsedId = Number(userId)
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [page, setPage] = useState(1)
  const canViewAuditLogs = useCanViewAuditLogs()

  const { data: users, isLoading: isLoadingUser } = useQuery(
    listUsersOptions({ query: { include_deleted: true } })
  )

  const target = useMemo(
    () => users?.find((u) => u.user.id === parsedId),
    [users, parsedId]
  )

  const { data: logs, isLoading: isLoadingLogs } = useQuery({
    ...listAuditLogsOptions({
      query: {
        limit: ITEMS_PER_PAGE,
        offset: (page - 1) * ITEMS_PER_PAGE,
        user_id: parsedId,
        operation_type: undefined,
        from: undefined,
        to: undefined,
      },
    }),
    enabled: Number.isFinite(parsedId) && canViewAuditLogs,
  })

  // Separate query for stats — pull a larger recent batch so we can
  // compute "last login" / "failed logins" / "actions (30d)" without
  // depending on the paginated view above.
  const { data: statsLogs } = useQuery({
    ...listAuditLogsOptions({
      query: {
        limit: STATS_BATCH_SIZE,
        offset: 0,
        user_id: parsedId,
        operation_type: undefined,
        from: undefined,
        to: undefined,
      },
    }),
    enabled: Number.isFinite(parsedId) && canViewAuditLogs,
  })

  const stats = useMemo(() => {
    if (!statsLogs) return null
    const now = Date.now()
    const cutoff = now - STATS_WINDOW_MS
    let actions30d = 0
    let failedLogins30d = 0
    let lastLogin: { at: number; city?: string; country?: string } | undefined
    let lastActivity: number | undefined
    for (const log of statsLogs) {
      if (lastActivity === undefined || log.audit_date > lastActivity) {
        lastActivity = log.audit_date
      }
      if (log.audit_date >= cutoff) {
        actions30d++
        if (log.operation_type === 'LOGIN_FAILURE') failedLogins30d++
      }
      if (
        log.operation_type === 'LOGIN_SUCCESS' &&
        (!lastLogin || log.audit_date > lastLogin.at)
      ) {
        lastLogin = {
          at: log.audit_date,
          city: log.ip_address?.city ?? undefined,
          country: log.ip_address?.country ?? undefined,
        }
      }
    }
    return { actions30d, failedLogins30d, lastLogin, lastActivity }
  }, [statsLogs])

  const userDisplayName = target?.user.name || target?.user.username || 'User'

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Users', href: '/settings/users' },
      { label: userDisplayName },
    ])
  }, [setBreadcrumbs, userDisplayName])

  usePageTitle(userDisplayName)

  const hasMore = useMemo(() => logs?.length === ITEMS_PER_PAGE, [logs])
  const showEmptyState = useMemo(
    () => !isLoadingLogs && (!logs || logs.length === 0),
    [isLoadingLogs, logs]
  )

  if (!Number.isFinite(parsedId)) {
    return (
      <div className="p-6">
        <EmptyState
          icon={ScrollText}
          title="Invalid user"
          description="The user id in the URL is not valid."
          action={
            <Button variant="outline" asChild>
              <Link to="/settings/users">
                <ArrowLeft className="mr-2 h-4 w-4" />
                Back to users
              </Link>
            </Button>
          }
        />
      </div>
    )
  }

  const lastLoginLocation = stats?.lastLogin
    ? [stats.lastLogin.city, stats.lastLogin.country].filter(Boolean).join(', ')
    : ''

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => navigate('/settings/users')}
        >
          <ArrowLeft className="mr-2 h-4 w-4" />
          Back to users
        </Button>
      </div>

      {/* Profile card */}
      <Card>
        <CardContent className="p-6">
          {isLoadingUser ? (
            <div className="flex items-center gap-4">
              <Skeleton className="h-16 w-16 rounded-full" />
              <div className="space-y-2">
                <Skeleton className="h-5 w-48" />
                <Skeleton className="h-4 w-64" />
              </div>
            </div>
          ) : !target ? (
            <EmptyState
              icon={ScrollText}
              title="User not found"
              description="This user may have been removed."
            />
          ) : (
            <div className="flex items-center gap-4">
              <Avatar className="h-16 w-16">
                <AvatarImage src={target.user.image} />
                <AvatarFallback className="text-lg">
                  {target.user.username?.slice(0, 2).toUpperCase() ||
                    target.user.name?.slice(0, 2).toUpperCase() ||
                    'U'}
                </AvatarFallback>
              </Avatar>
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2">
                  <h1 className="truncate text-xl font-semibold">
                    {target.user.name || target.user.username}
                  </h1>
                  {Array.from(
                    new Map(target.roles.map((r) => [r.name, r])).values()
                  ).map((role) => (
                    <Badge
                      key={role.id}
                      variant="secondary"
                      className="text-xs"
                    >
                      {role.name}
                    </Badge>
                  ))}
                  {target.user.mfa_enabled && (
                    <Badge
                      variant="secondary"
                      className="gap-1 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
                    >
                      <Shield className="h-3 w-3" />
                      MFA
                    </Badge>
                  )}
                  {target.user.email_verified ? (
                    <Badge
                      variant="secondary"
                      className="gap-1 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
                    >
                      <MailCheck className="h-3 w-3" />
                      Verified
                    </Badge>
                  ) : (
                    <Badge
                      variant="secondary"
                      className="gap-1 bg-amber-500/10 text-amber-600 dark:text-amber-400"
                    >
                      <MailWarning className="h-3 w-3" />
                      Unverified
                    </Badge>
                  )}
                  {target.user.deleted_at && (
                    <Badge variant="destructive" className="text-xs">
                      Deleted
                    </Badge>
                  )}
                </div>
                <div className="mt-1 flex items-center gap-1.5 text-sm text-muted-foreground">
                  <Mail className="h-3.5 w-3.5" />
                  <span className="truncate">{target.user.email}</span>
                </div>
                {target.user.username &&
                  target.user.username !== target.user.name && (
                    <div className="mt-0.5 text-xs text-muted-foreground">
                      @{target.user.username}
                    </div>
                  )}
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      {/* Stats strip — derived from recent audit logs + user record */}
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard
          icon={<Calendar className="h-4 w-4" />}
          label="Member since"
          value={
            target ? format(new Date(target.user.created_at), 'PP') : undefined
          }
          hint={
            target
              ? `${formatDistanceToNow(new Date(target.user.created_at))} ago`
              : undefined
          }
          loading={isLoadingUser}
        />
        {canViewAuditLogs && (
          <>
            <StatCard
              icon={<LogIn className="h-4 w-4" />}
              label="Last login"
              value={
                stats?.lastLogin
                  ? formatDistanceToNow(new Date(stats.lastLogin.at), {
                      addSuffix: true,
                    })
                  : statsLogs
                    ? 'Never'
                    : undefined
              }
              hint={
                stats?.lastLogin
                  ? lastLoginLocation ||
                    format(new Date(stats.lastLogin.at), 'PP p')
                  : undefined
              }
              loading={!statsLogs}
            />
            <StatCard
              icon={<Activity className="h-4 w-4" />}
              label="Actions (30d)"
              value={
                stats
                  ? stats.actions30d >= STATS_BATCH_SIZE
                    ? `${STATS_BATCH_SIZE}+`
                    : String(stats.actions30d)
                  : undefined
              }
              hint={
                stats?.lastActivity
                  ? `last ${formatDistanceToNow(
                      new Date(stats.lastActivity)
                    )} ago`
                  : undefined
              }
              loading={!statsLogs}
            />
            <StatCard
              icon={
                stats && stats.failedLogins30d > 0 ? (
                  <AlertTriangle className="h-4 w-4 text-amber-500" />
                ) : (
                  <Clock className="h-4 w-4" />
                )
              }
              label="Failed logins (30d)"
              value={stats ? String(stats.failedLogins30d) : undefined}
              hint={
                stats && stats.failedLogins30d === 0
                  ? 'No failed attempts'
                  : undefined
              }
              tone={stats && stats.failedLogins30d > 0 ? 'warning' : undefined}
              loading={!statsLogs}
            />
          </>
        )}
      </div>

      {/* Audit log section — reuses AuditLogItemRow to match /audit rendering.
          Gated to audit-log viewers: only administration roles may read
          another user's activity. */}
      {canViewAuditLogs && (
        <div className="space-y-3">
          <div className="flex flex-col gap-1">
            <h2 className="text-lg font-semibold">Audit logs</h2>
            <p className="text-sm text-muted-foreground">
              Actions performed by {userDisplayName}.
            </p>
          </div>

          <Card>
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-8" />
                    <TableHead className="w-[110px]">Type</TableHead>
                    <TableHead>Operation</TableHead>
                    <TableHead className="hidden md:table-cell">
                      Actor
                    </TableHead>
                    <TableHead className="hidden lg:table-cell">
                      Origin
                    </TableHead>
                    <TableHead className="text-right">When</TableHead>
                    <TableHead className="w-8" />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {isLoadingLogs ? (
                    Array.from({ length: 6 }).map((_, i) => (
                      <TableRow key={i}>
                        <TableCell />
                        <TableCell>
                          <Skeleton className="h-5 w-16" />
                        </TableCell>
                        <TableCell>
                          <Skeleton className="h-4 w-64" />
                        </TableCell>
                        <TableCell className="hidden md:table-cell">
                          <Skeleton className="h-4 w-24" />
                        </TableCell>
                        <TableCell className="hidden lg:table-cell">
                          <Skeleton className="h-4 w-32" />
                        </TableCell>
                        <TableCell className="text-right">
                          <Skeleton className="ml-auto h-4 w-32" />
                        </TableCell>
                        <TableCell />
                      </TableRow>
                    ))
                  ) : showEmptyState ? (
                    <TableRow className="hover:bg-transparent">
                      <TableCell colSpan={7} className="p-0">
                        <EmptyState
                          icon={ScrollText}
                          title="No activity yet"
                          description="Actions performed by this user will show up here."
                        />
                      </TableCell>
                    </TableRow>
                  ) : (
                    logs?.map((log) => (
                      <AuditLogItemRow
                        key={log.id}
                        id={log.id}
                        operation_type={log.operation_type}
                        audit_date={log.audit_date}
                        user={log.user ?? undefined}
                        ip_address={log.ip_address ?? undefined}
                        data={log.data as Record<string, unknown> | undefined}
                      />
                    ))
                  )}
                </TableBody>
              </Table>
            </div>
          </Card>

          {!showEmptyState && (
            <div className="flex items-center justify-between">
              <p className="text-sm text-muted-foreground">
                Page {page}
                {logs &&
                  ` · ${logs.length} result${logs.length === 1 ? '' : 's'}`}
              </p>
              <div className="flex gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setPage((p) => Math.max(1, p - 1))}
                  disabled={page === 1 || isLoadingLogs}
                >
                  Previous
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setPage((p) => p + 1)}
                  disabled={!hasMore || isLoadingLogs}
                >
                  Next
                </Button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  )
}

interface StatCardProps {
  icon: ReactNode
  label: string
  value?: string
  hint?: string
  loading?: boolean
  tone?: 'warning'
}

function StatCard({ icon, label, value, hint, loading, tone }: StatCardProps) {
  return (
    <Card>
      <CardContent className="p-4">
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          {icon}
          <span>{label}</span>
        </div>
        {loading ? (
          <Skeleton className="mt-2 h-6 w-24" />
        ) : (
          <div
            className={
              tone === 'warning'
                ? 'mt-1 text-lg font-semibold text-amber-600 dark:text-amber-400'
                : 'mt-1 text-lg font-semibold'
            }
          >
            {value ?? '—'}
          </div>
        )}
        {hint && !loading && (
          <div className="mt-0.5 text-xs text-muted-foreground">{hint}</div>
        )}
      </CardContent>
    </Card>
  )
}
