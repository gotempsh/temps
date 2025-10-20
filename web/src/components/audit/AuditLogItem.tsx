import { AuditLogIpInfo, AuditLogUserInfo } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { format } from 'date-fns'
import { ChevronDown } from 'lucide-react'
import { Link } from 'react-router-dom'

interface AuditLogItemProps {
  id: number
  operation_type: AuditOperationType
  audit_date: number
  user: AuditLogUserInfo
  ip_address: AuditLogIpInfo
  data?: Record<string, string>
}
type AuditOperationType =
  // Authentication & User Management (temps-auth)
  | 'LOGIN_SUCCESS'
  | 'LOGIN_FAILURE'
  | 'USER_CREATED'
  | 'USER_UPDATED'
  | 'USER_DELETED'
  | 'USER_RESTORED'
  | 'USER_LOGOUT'
  | 'ROLE_ASSIGNED'
  | 'ROLE_REMOVED'
  | 'MFA_ENABLED'
  | 'MFA_DISABLED'
  | 'MFA_VERIFIED'
  // External Service Management (temps-providers)
  | 'EXTERNAL_SERVICE_CREATED'
  | 'EXTERNAL_SERVICE_UPDATED'
  | 'EXTERNAL_SERVICE_DELETED'
  | 'EXTERNAL_SERVICE_STATUS_CHANGED'
  | 'EXTERNAL_SERVICE_PROJECT_LINKED'
  | 'EXTERNAL_SERVICE_PROJECT_UNLINKED'
  // Project Management (temps-projects)
  | 'PROJECT_CREATED'
  | 'PROJECT_UPDATED'
  | 'PROJECT_DELETED'
  | 'PROJECT_GITHUB_UPDATED'
  | 'PROJECT_SETTINGS_UPDATED'
  | 'ENVIRONMENT_SETTINGS_UPDATED'
  // Backup & Storage (temps-backup)
  | 'S3_SOURCE_CREATED'
  | 'S3_SOURCE_UPDATED'
  | 'S3_SOURCE_DELETED'
  | 'BACKUP_SCHEDULE_STATUS_CHANGED'
  | 'BACKUP_RUN'
  // Pipeline & Git (temps-git)
  | 'PIPELINE_TRIGGERED'
const getAuditDescription = (
  operation_type: AuditOperationType,
  data?: Record<string, string>,
  user?: AuditLogUserInfo
) => {
  switch (operation_type) {
    // Authentication & User Management (temps-auth)
    case 'LOGIN_SUCCESS':
      return 'Logged in successfully'
    case 'LOGIN_FAILURE':
      return 'Failed login attempt'
    case 'USER_CREATED':
      return `Created user account for ${data?.username || 'a user'}`
    case 'USER_UPDATED':
      return `Updated user account for ${data?.username || 'a user'}`
    case 'USER_DELETED':
      return `Deleted user account for ${data?.username || 'a user'}`
    case 'USER_RESTORED':
      return `Restored user account for ${data?.username || 'a user'}`
    case 'USER_LOGOUT':
      return 'Logged out'
    case 'ROLE_ASSIGNED':
      return `Assigned role ${data?.role || 'unknown'} to ${data?.username || 'a user'}`
    case 'ROLE_REMOVED':
      return `Removed role ${data?.role || 'unknown'} from ${data?.username || 'a user'}`
    case 'MFA_ENABLED':
      return `${user?.name} enabled multi-factor authentication`
    case 'MFA_DISABLED':
      return `${user?.name} disabled multi-factor authentication`
    case 'MFA_VERIFIED':
      return `${user?.name} verified multi-factor authentication`

    // External Service Management (temps-providers)
    case 'EXTERNAL_SERVICE_CREATED':
      return `Created external service${data?.service_name ? ` "${data.service_name}"` : ''}`
    case 'EXTERNAL_SERVICE_UPDATED':
      return `Updated external service${data?.service_name ? ` "${data.service_name}"` : ''}`
    case 'EXTERNAL_SERVICE_DELETED':
      return `Deleted external service${data?.service_name ? ` "${data.service_name}"` : ''}`
    case 'EXTERNAL_SERVICE_STATUS_CHANGED':
      return `Changed status of external service${data?.service_name ? ` "${data.service_name}"` : ''} to ${data?.status || 'unknown'}`
    case 'EXTERNAL_SERVICE_PROJECT_LINKED':
      return data?.project_slug ? (
        <>
          Linked external service to project{' '}
          <Link
            to={`/projects/${data.project_slug}`}
            className="text-primary hover:underline"
          >
            {data.project_slug}
          </Link>
        </>
      ) : (
        'Linked external service to a project'
      )
    case 'EXTERNAL_SERVICE_PROJECT_UNLINKED':
      return data?.project_slug ? (
        <>
          Unlinked external service from project{' '}
          <Link
            to={`/projects/${data.project_slug}`}
            className="text-primary hover:underline"
          >
            {data.project_slug}
          </Link>
        </>
      ) : (
        'Unlinked external service from a project'
      )

    // Project Management (temps-projects)
    case 'PROJECT_CREATED':
      return data?.project_slug ? (
        <>
          Created project{' '}
          <Link
            to={`/projects/${data.project_slug}`}
            className="text-primary hover:underline"
          >
            {data.project_slug}
          </Link>
        </>
      ) : (
        'Created a new project'
      )
    case 'PROJECT_UPDATED':
      return data?.project_slug ? (
        <>
          Updated project{' '}
          <Link
            to={`/projects/${data.project_slug}`}
            className="text-primary hover:underline"
          >
            {data.project_slug}
          </Link>
        </>
      ) : (
        'Updated a project'
      )
    case 'PROJECT_DELETED':
      return `Deleted project ${data?.project_slug || 'unknown'}`
    case 'PROJECT_GITHUB_UPDATED':
      return data?.project_slug ? (
        <>
          Updated GitHub settings for{' '}
          <Link
            to={`/projects/${data.project_slug}`}
            className="text-primary hover:underline"
          >
            {data.project_slug}
          </Link>
        </>
      ) : (
        'Updated project GitHub settings'
      )
    case 'PROJECT_SETTINGS_UPDATED':
      return data?.project_slug ? (
        <>
          Updated settings for{' '}
          <Link
            to={`/projects/${data.project_slug}`}
            className="text-primary hover:underline"
          >
            {data.project_slug}
          </Link>
        </>
      ) : (
        'Updated project settings'
      )
    case 'ENVIRONMENT_SETTINGS_UPDATED':
      return data?.project_slug ? (
        <>
          Updated environment settings for{' '}
          <Link
            to={`/projects/${data.project_slug}`}
            className="text-primary hover:underline"
          >
            {data.project_slug}
          </Link>
        </>
      ) : (
        'Updated environment settings'
      )

    // Backup & Storage (temps-backup)
    case 'S3_SOURCE_CREATED':
      return `Created S3 source${data?.source_name ? ` "${data.source_name}"` : ''}`
    case 'S3_SOURCE_UPDATED':
      return `Updated S3 source${data?.source_name ? ` "${data.source_name}"` : ''}`
    case 'S3_SOURCE_DELETED':
      return `Deleted S3 source${data?.source_name ? ` "${data.source_name}"` : ''}`
    case 'BACKUP_SCHEDULE_STATUS_CHANGED':
      return `Changed backup schedule status to ${data?.status || 'unknown'}`
    case 'BACKUP_RUN':
      return `Ran backup${data?.backup_id ? ` (ID: ${data.backup_id})` : ''}`

    // Pipeline & Git (temps-git)
    case 'PIPELINE_TRIGGERED':
      return data?.project_slug ? (
        <>
          Triggered pipeline for{' '}
          <Link
            to={`/projects/${data.project_slug}`}
            className="text-primary hover:underline underline"
          >
            {data?.project_slug || 'a project'}
          </Link>
        </>
      ) : (
        `Triggered pipeline for ${data?.project_slug || 'a project'}`
      )

    // Default
    default:
      return 'Performed unknown operation'
  }
}

export function AuditLogItem({
  operation_type,
  audit_date,
  user,
  ip_address,
  data,
}: AuditLogItemProps) {
  return (
    <Card className="p-4">
      <Collapsible>
        <div className="flex justify-between">
          <div>
            <p className="font-medium">
              {getAuditDescription(operation_type, data, user)}
            </p>
            {ip_address && (
              <p className="text-sm text-muted-foreground">
                <span className="flex items-center gap-1">
                  <span>{ip_address.ip}</span>
                  {(ip_address.city || ip_address.country) && (
                    <span className="text-muted-foreground">
                      (
                      {[ip_address.city, ip_address.country]
                        .filter(Boolean)
                        .join(', ')}
                      )
                    </span>
                  )}
                </span>
              </p>
            )}
          </div>
          <div className="flex flex-col items-end gap-1">
            <p className="text-sm text-muted-foreground">
              {format(new Date(audit_date), 'PPpp')}
            </p>
            <p className="text-sm">{user?.name}</p>
            {data && Object.keys(data).length > 0 && (
              <CollapsibleTrigger asChild>
                <Button variant="ghost" size="sm" className="h-6 px-2">
                  <ChevronDown className="h-4 w-4" />
                  <span className="sr-only">Toggle data details</span>
                </Button>
              </CollapsibleTrigger>
            )}
          </div>
        </div>
        {data && Object.keys(data).length > 0 && (
          <CollapsibleContent>
            <div className="mt-4 rounded-md bg-muted p-4">
              <div className="space-y-2">
                {Object.entries(data).map(([key, value]) => (
                  <div key={key} className="flex">
                    <span className="w-[180px] shrink-0 font-medium text-muted-foreground">
                      {key}:
                    </span>
                    <pre className="flex-1 whitespace-pre-wrap text-sm">
                      {typeof value === 'object'
                        ? JSON.stringify(value, null, 2)
                        : String(value)}
                    </pre>
                  </div>
                ))}
              </div>
            </div>
          </CollapsibleContent>
        )}
      </Collapsible>
    </Card>
  )
}
