import {
  deleteAlertRuleMutation,
  listAlertRulesOptions,
  updateAlertRuleMutation,
  getProjectsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AlertRuleResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { Badge } from '@/components/ui/badge'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  EllipsisVertical,
  Plus,
  ShieldAlert,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'

const TRIGGER_TYPES = [
  { value: 'new_issue', label: 'New Issue' },
  { value: 'regression', label: 'Regression' },
  { value: 'frequency', label: 'Frequency' },
  { value: 'new_user', label: 'New User Affected' },
  { value: 'user_count', label: 'User Count' },
  { value: 'status_change', label: 'Status Change' },
] as const

function triggerTypeLabel(type: string): string {
  return TRIGGER_TYPES.find((t) => t.value === type)?.label ?? type
}

function priorityVariant(
  priority: string
): 'default' | 'secondary' | 'destructive' | 'outline' {
  switch (priority) {
    case 'Critical':
      return 'destructive'
    case 'High':
      return 'default'
    case 'Normal':
      return 'secondary'
    default:
      return 'outline'
  }
}

function renderTriggerConfig(rule: AlertRuleResponse) {
  const config = (rule.trigger_config ?? {}) as Record<string, unknown>
  switch (rule.trigger_type) {
    case 'frequency':
      return (
        <p>
          Threshold: {String(config.count ?? '—')} events /{' '}
          {String(config.window_minutes ?? '—')} min
        </p>
      )
    case 'user_count':
      return <p>User threshold: {String(config.threshold ?? '—')}</p>
    default:
      return null
  }
}

interface AlertRulesManagementProps {
  projectId?: number
}

export function AlertRulesManagement({
  projectId: fixedProjectId,
}: AlertRulesManagementProps = {}) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [selectedProjectId, setSelectedProjectId] = useState<number | null>(
    null
  )

  const { data: projects, isLoading: projectsLoading } = useQuery({
    ...getProjectsOptions(),
    enabled: !fixedProjectId,
  })

  const projectList = projects?.projects
  const projectId =
    fixedProjectId ?? selectedProjectId ?? projectList?.[0]?.id ?? null
  const selectedProject = projectList?.find(
    (project) => project.id === projectId
  )
  const showProjectSelector = !fixedProjectId && (projectList?.length ?? 0) > 1

  const navigateToRule = (suffix: 'new' | `${number}/edit`) => {
    if (fixedProjectId) {
      navigate(suffix)
      return
    }

    if (!selectedProject) return
    navigate(`/projects/${selectedProject.slug}/errors/alert-rules/${suffix}`)
  }

  const { data: rules, isLoading: rulesLoading } = useQuery({
    ...listAlertRulesOptions({
      path: { project_id: projectId! },
    }),
    enabled: !!projectId,
  })

  const updateMutation = useMutation({
    ...updateAlertRuleMutation(),
    meta: { errorTitle: 'Failed to update alert rule' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as Record<string, unknown>)?._id ===
          'listAlertRules',
      })
    },
  })

  const deleteMutation = useMutation({
    ...deleteAlertRuleMutation(),
    meta: { errorTitle: 'Failed to delete alert rule' },
    onSuccess: () => {
      toast.success('Alert rule deleted')
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as Record<string, unknown>)?._id ===
          'listAlertRules',
      })
    },
  })

  const handleDelete = async (rule: AlertRuleResponse) => {
    if (!projectId) return
    await deleteMutation.mutateAsync({
      path: { project_id: projectId, rule_id: rule.id },
    })
  }

  const handleToggleEnabled = async (rule: AlertRuleResponse) => {
    if (!projectId) return
    await updateMutation.mutateAsync({
      path: { project_id: projectId, rule_id: rule.id },
      body: { enabled: !rule.enabled },
    })
  }

  const hasRules = useMemo(() => rules && rules.length > 0, [rules])

  if (!fixedProjectId && projectsLoading) {
    return (
      <div className="flex items-center justify-center py-6">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary" />
      </div>
    )
  }

  if (!fixedProjectId && !projectList?.length) {
    return (
      <EmptyState
        icon={ShieldAlert}
        title="No projects found"
        description="Create a project first to configure error alert rules."
      />
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h3 className="text-lg font-medium">Error Alert Rules</h3>
          <p className="text-sm text-muted-foreground">
            Configure rules that trigger notifications when errors match certain
            conditions.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {showProjectSelector && (
            <Select
              value={String(projectId)}
              onValueChange={(v) => setSelectedProjectId(Number(v))}
            >
              <SelectTrigger className="w-[200px]">
                <SelectValue placeholder="Select project" />
              </SelectTrigger>
              <SelectContent>
                {projectList!.map((p) => (
                  <SelectItem key={p.id} value={String(p.id)}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <CreateActionButton
            onClick={() => navigateToRule('new')}
            disabled={!projectId}
            label="Add Rule"
          />
        </div>
      </div>

      {rulesLoading ? (
        <div className="flex items-center justify-center py-6">
          <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary" />
        </div>
      ) : !hasRules ? (
        <EmptyState
          icon={AlertTriangle}
          title="No alert rules configured"
          description="Create your first error alert rule to get notified when errors match specific conditions."
          action={
            <Button onClick={() => navigateToRule('new')}>
              <Plus className="h-4 w-4 mr-2" />
              Add Rule
            </Button>
          }
        />
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {rules?.map((rule) => (
            <Card
              key={rule.id}
              className="cursor-pointer hover:border-primary/50 transition-colors"
              onClick={() => navigateToRule(`${rule.id}/edit`)}
            >
              <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
                <div className="space-y-1 min-w-0 flex-1">
                  <CardTitle className="text-base font-medium leading-none truncate">
                    {rule.name}
                  </CardTitle>
                  <p className="text-xs text-muted-foreground">
                    {triggerTypeLabel(rule.trigger_type)}
                  </p>
                </div>
                <div
                  className="flex items-center gap-1 shrink-0"
                  onClick={(e) => e.stopPropagation()}
                >
                  <Switch
                    checked={rule.enabled}
                    onCheckedChange={() => handleToggleEnabled(rule)}
                    disabled={updateMutation.isPending}
                  />
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button variant="ghost" size="icon" className="h-8 w-8">
                        <EllipsisVertical className="h-4 w-4" />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        onClick={() => navigateToRule(`${rule.id}/edit`)}
                      >
                        Edit
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        className="text-destructive"
                        onClick={() => handleDelete(rule)}
                      >
                        Delete
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              </CardHeader>
              <CardContent className="space-y-3">
                <div className="flex items-center gap-2 flex-wrap">
                  <Badge variant={priorityVariant(rule.notification_priority)}>
                    {rule.notification_priority}
                  </Badge>
                  <Badge variant="outline">
                    {triggerTypeLabel(rule.trigger_type)}
                  </Badge>
                  {rule.error_level_filter && (
                    <Badge variant="secondary">{rule.error_level_filter}</Badge>
                  )}
                </div>
                <div className="space-y-1 text-xs text-muted-foreground">
                  <p>Cooldown: {rule.cooldown_minutes} min</p>
                  {renderTriggerConfig(rule)}
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  )
}
