import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import {
  createAgentMutation,
  listAgentsQueryKey,
  listGlobalMcpsOptions,
  listGlobalSkillsOptions,
  listMcpsOptions,
  listSkillsOptions,
  updateAgentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  AgentConfigResponse,
  UpsertAgentRequest,
} from '@/api/client/types.gen'

export interface TriggerConfig {
  error?: { new_issue?: boolean; regression?: boolean }
  deploy?: { production?: boolean; preview?: boolean }
  monitoring?: { downtime?: boolean; latency_spike?: boolean }
  schedule?: { cron?: string | null }
  manual?: boolean
  webhook?: boolean
}

export type Agent = Omit<AgentConfigResponse, 'trigger_config' | 'skills_config' | 'mcp_servers_config'> & {
  trigger_config?: TriggerConfig | null
  skills_config?: string[] | null
  mcp_servers_config?: string[] | null
}

interface AgentSettingsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  projectId: number
  agent?: Agent | null
}

export function AgentSettingsDialog({
  open,
  onOpenChange,
  projectId,
  agent,
}: AgentSettingsDialogProps) {
  const isEdit = !!agent
  const queryClient = useQueryClient()

  const [slug, setSlug] = useState(agent?.slug ?? '')
  const [name, setName] = useState(agent?.name ?? '')
  const [description, setDescription] = useState(agent?.description ?? '')
  const [enabled, setEnabled] = useState(agent?.enabled ?? false)
  const [aiProvider, setAiProvider] = useState(agent?.ai_provider ?? '')
  const [aiModel, setAiModel] = useState<string>(agent?.ai_model ?? '')
  const [prompt, setPrompt] = useState(agent?.prompt ?? '')
  const [maxTurns, setMaxTurns] = useState(agent?.max_turns ?? 25)
  const [timeoutSeconds, setTimeoutSeconds] = useState(agent?.timeout_seconds ?? 600)
  const [dailyBudgetCents, setDailyBudgetCents] = useState(agent?.daily_budget_cents ?? 500)
  const [cooldownMinutes, setCooldownMinutes] = useState(agent?.cooldown_minutes ?? 30)
  const [branchPrefix, setBranchPrefix] = useState(agent?.branch_prefix ?? 'agents/')
  const [deliverable, setDeliverable] = useState(agent?.deliverable ?? 'pull_request')
  const [configRepoUrl, setConfigRepoUrl] = useState(agent?.config_repo_url ?? '')
  const [configRepoBranch, setConfigRepoBranch] = useState(agent?.config_repo_branch ?? '')

  // Skills & MCP (slug arrays referencing project-level definitions)
  const [selectedSkills, setSelectedSkills] = useState<string[]>(
    agent?.skills_config ?? [],
  )
  const [selectedMcps, setSelectedMcps] = useState<string[]>(
    agent?.mcp_servers_config ?? [],
  )

  // Triggers. Defaults are `false` so a workflow whose YAML omits a trigger
  // (e.g. a daily report with only `schedule.cron`) does not render its
  // unrelated toggles as "on" — and, crucially, can't accidentally persist
  // them on save. The previous `?? true` defaults silently enabled error /
  // regression / manual triggers for every workflow that lacked them.
  const [triggerNewIssue, setTriggerNewIssue] = useState(
    agent?.trigger_config?.error?.new_issue ?? false,
  )
  const [triggerRegression, setTriggerRegression] = useState(
    agent?.trigger_config?.error?.regression ?? false,
  )
  const [triggerManual, setTriggerManual] = useState(
    agent?.trigger_config?.manual ?? false,
  )
  const [triggerCron, setTriggerCron] = useState(
    agent?.trigger_config?.schedule?.cron ?? '',
  )

  // Reset form when dialog opens with different agent
  const resetForm = () => {
    setSlug(agent?.slug ?? '')
    setName(agent?.name ?? '')
    setDescription(agent?.description ?? '')
    setEnabled(agent?.enabled ?? false)
    setAiProvider(agent?.ai_provider ?? '')
    setAiModel(agent?.ai_model ?? '')
    setPrompt(agent?.prompt ?? '')
    setMaxTurns(agent?.max_turns ?? 25)
    setTimeoutSeconds(agent?.timeout_seconds ?? 600)
    setDailyBudgetCents(agent?.daily_budget_cents ?? 500)
    setCooldownMinutes(agent?.cooldown_minutes ?? 30)
    setBranchPrefix(agent?.branch_prefix ?? 'agents/')
    setDeliverable(agent?.deliverable ?? 'pull_request')
    setConfigRepoUrl(agent?.config_repo_url ?? '')
    setConfigRepoBranch(agent?.config_repo_branch ?? '')
    setSelectedSkills(agent?.skills_config ?? [])
    setSelectedMcps(agent?.mcp_servers_config ?? [])
    setTriggerNewIssue(agent?.trigger_config?.error?.new_issue ?? false)
    setTriggerRegression(agent?.trigger_config?.error?.regression ?? false)
    setTriggerManual(agent?.trigger_config?.manual ?? false)
    setTriggerCron(agent?.trigger_config?.schedule?.cron ?? '')
  }

  // Reset form when agent changes (opening dialog for a different agent)
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => { resetForm() }, [agent?.id, open])

  // Fetch the AI provider catalog so we can render a model picker driven by
  // the same `models` list the settings page uses. Empty `models` for a
  // provider (e.g. OpenCode) hides the dropdown — that provider picks its own.
  const { data: providerCatalog } = useQuery({
    queryKey: ['ai-provider-catalog'],
    queryFn: async () => {
      const res = await fetch('/api/settings/ai-providers')
      if (!res.ok) return null
      return res.json() as Promise<{
        default_provider: string
        providers: Array<{ id: string; models: string[]; default_model: string | null }>
      }>
    },
    enabled: open,
    staleTime: 60 * 1000,
  })

  const availableModels =
    providerCatalog?.providers.find((p) => p.id === aiProvider)?.models ?? []

  // When creating a new workflow and the catalog loads, sync the provider
  // dropdown to the platform's configured default instead of hardcoding claude_cli.
  useEffect(() => {
    if (!isEdit && providerCatalog?.default_provider) {
      setAiProvider((prev) =>
        prev === '' ? providerCatalog.default_provider : prev,
      )
    }
  }, [isEdit, providerCatalog?.default_provider])

  // Fetch available definitions: project-level + global
  const { data: projectSkillsData } = useQuery({
    ...listSkillsOptions({ path: { project_id: projectId } }),
    enabled: open,
  })
  const { data: globalSkillsData } = useQuery({
    ...listGlobalSkillsOptions(),
    enabled: open,
  })
  const projectSkills = projectSkillsData?.items ?? []
  const globalSkills = globalSkillsData?.items ?? []
  const availableSkills = [
    ...projectSkills,
    ...globalSkills.filter((g) => !projectSkills.some((p) => p.slug === g.slug)),
  ]

  const { data: projectMcpsData } = useQuery({
    ...listMcpsOptions({ path: { project_id: projectId } }),
    enabled: open,
  })
  const { data: globalMcpsData } = useQuery({
    ...listGlobalMcpsOptions(),
    enabled: open,
  })
  const projectMcps = projectMcpsData?.items ?? []
  const globalMcps = globalMcpsData?.items ?? []
  const availableMcps = [
    ...projectMcps,
    ...globalMcps.filter((g) => !projectMcps.some((p) => p.slug === g.slug)),
  ]

  const toggleSkill = (slug: string) => {
    setSelectedSkills((prev) =>
      prev.includes(slug) ? prev.filter((s) => s !== slug) : [...prev, slug],
    )
  }

  const toggleMcp = (slug: string) => {
    setSelectedMcps((prev) =>
      prev.includes(slug) ? prev.filter((s) => s !== slug) : [...prev, slug],
    )
  }

  const createMutation = useMutation({
    ...createAgentMutation(),
    onSuccess: () => {
      toast.success('Workflow created')
      queryClient.invalidateQueries({
        queryKey: listAgentsQueryKey({ path: { project_id: projectId } }),
      })
      onOpenChange(false)
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to create workflow')
    },
  })

  const updateMutation = useMutation({
    ...updateAgentMutation(),
    onSuccess: () => {
      toast.success('Workflow updated')
      queryClient.invalidateQueries({
        queryKey: listAgentsQueryKey({ path: { project_id: projectId } }),
      })
      onOpenChange(false)
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to update workflow')
    },
  })

  const isPending = createMutation.isPending || updateMutation.isPending

  const handleSubmit = () => {
    // Only include trigger blocks the user actually enabled. Writing
    // `error: { new_issue: false, regression: false }` and `manual: false`
    // unconditionally caused workflows to persist triggers they never asked
    // for — and a later read+save round-trip with the old `?? true` defaults
    // could then flip them on.
    const triggerConfig: Record<string, unknown> = {}
    if (triggerNewIssue || triggerRegression) {
      triggerConfig.error = {
        new_issue: triggerNewIssue,
        regression: triggerRegression,
      }
    }
    if (triggerManual) {
      triggerConfig.manual = true
    }
    if (triggerCron.trim()) {
      triggerConfig.schedule = { cron: triggerCron.trim() }
    }

    if (isEdit) {
      const body: UpsertAgentRequest = {
        slug: agent!.slug,
        name,
        description: description || undefined,
        enabled,
        ai_provider: aiProvider,
        ai_model: aiModel === '' ? null : aiModel,
        trigger_config: triggerConfig,
        prompt: prompt || undefined,
        max_turns: maxTurns,
        timeout_seconds: timeoutSeconds,
        daily_budget_cents: dailyBudgetCents,
        cooldown_minutes: cooldownMinutes,
        branch_prefix: branchPrefix,
        deliverable,
        config_repo_url: configRepoUrl || null,
        config_repo_branch: configRepoBranch || null,
        mcp_servers_config: selectedMcps.length > 0 ? selectedMcps : null,
        skills_config: selectedSkills.length > 0 ? selectedSkills : null,
      }
      updateMutation.mutate({
        path: { project_id: projectId, slug: agent!.slug },
        body,
      })
    } else {
      if (!slug.trim()) {
        toast.error('Slug is required')
        return
      }
      if (!name.trim()) {
        toast.error('Name is required')
        return
      }
      const body: UpsertAgentRequest = {
        slug: slug.trim(),
        name: name.trim(),
        description: description || undefined,
        enabled,
        ai_provider: aiProvider,
        ai_model: aiModel === '' ? null : aiModel,
        trigger_config: triggerConfig,
        prompt: prompt || undefined,
        max_turns: maxTurns,
        timeout_seconds: timeoutSeconds,
        daily_budget_cents: dailyBudgetCents,
        cooldown_minutes: cooldownMinutes,
        branch_prefix: branchPrefix,
        deliverable,
        config_repo_url: configRepoUrl || undefined,
        config_repo_branch: configRepoBranch || undefined,
        mcp_servers_config: selectedMcps.length > 0 ? selectedMcps : undefined,
        skills_config: selectedSkills.length > 0 ? selectedSkills : undefined,
      }
      createMutation.mutate({
        path: { project_id: projectId },
        body,
      })
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(v) => {
        if (v) resetForm()
        onOpenChange(v)
      }}
    >
      <DialogContent className="max-w-lg max-h-[85vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{isEdit ? 'Edit Workflow' : 'Create Workflow'}</DialogTitle>
        </DialogHeader>

        <form onSubmit={(e) => { e.preventDefault(); handleSubmit() }} className="space-y-6 py-4">
          {/* Basic info */}
          <div className="space-y-3">
            {!isEdit && (
              <div className="space-y-1.5">
                <Label htmlFor="slug">Slug</Label>
                <Input
                  id="slug"
                  value={slug}
                  onChange={(e) => setSlug(e.target.value)}
                  placeholder="error-fixer"
                />
                <p className="text-xs text-muted-foreground">
                  Unique identifier. Used in URLs and YAML config.
                </p>
              </div>
            )}
            <div className="space-y-1.5">
              <Label htmlFor="name">Name</Label>
              <Input
                id="name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="Error Fixer"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="description">Description</Label>
              <Input
                id="description"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="Automatically fixes production errors"
              />
            </div>
            <div className="flex items-center justify-between">
              <Label htmlFor="enabled">Enabled</Label>
              <Switch
                id="enabled"
                checked={enabled}
                onCheckedChange={setEnabled}
              />
            </div>
          </div>

          {/* Triggers */}
          <div className="space-y-3">
            <h3 className="text-sm font-medium">Triggers</h3>
            <div className="flex items-center justify-between">
              <Label htmlFor="trigger-new-issue" className="text-sm font-normal">
                New errors
              </Label>
              <Switch
                id="trigger-new-issue"
                checked={triggerNewIssue}
                onCheckedChange={setTriggerNewIssue}
              />
            </div>
            <div className="flex items-center justify-between">
              <Label htmlFor="trigger-regression" className="text-sm font-normal">
                Regressions
              </Label>
              <Switch
                id="trigger-regression"
                checked={triggerRegression}
                onCheckedChange={setTriggerRegression}
              />
            </div>
            <div className="flex items-center justify-between">
              <Label htmlFor="trigger-manual" className="text-sm font-normal">
                Manual trigger
              </Label>
              <Switch
                id="trigger-manual"
                checked={triggerManual}
                onCheckedChange={setTriggerManual}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="trigger-cron" className="text-sm font-normal">
                Schedule (cron)
              </Label>
              <Input
                id="trigger-cron"
                placeholder="e.g. 0 * * * * (every hour)"
                value={triggerCron}
                onChange={(e) => setTriggerCron(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                Leave empty for no schedule. Uses standard cron syntax.
              </p>
            </div>
          </div>

          {/* Deliverable */}
          <div className="space-y-3">
            <h3 className="text-sm font-medium">Deliverable</h3>
            <Select value={deliverable} onValueChange={setDeliverable}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="pull_request">Pull Request</SelectItem>
                <SelectItem value="report">Report</SelectItem>
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">
              {deliverable === 'report'
                ? 'Workflow produces a report — no branch, PR, or deployment.'
                : 'Workflow pushes a branch, creates a PR, and triggers a preview deployment.'}
            </p>
          </div>

          {/* AI Provider */}
          <div className="space-y-3">
            <h3 className="text-sm font-medium">AI Provider</h3>
            <div className="space-y-1.5">
              <Label htmlFor="ai-provider">Provider</Label>
              <Select
                value={aiProvider}
                onValueChange={(v) => {
                  setAiProvider(v)
                  // Reset model when switching provider — the catalog model
                  // lists don't overlap (e.g. "sonnet" vs "gpt-5-codex").
                  setAiModel('')
                }}
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="claude_cli">Claude Code</SelectItem>
                  <SelectItem value="opencode">OpenCode</SelectItem>
                  <SelectItem value="codex_cli">Codex</SelectItem>
                </SelectContent>
              </Select>
            </div>
            {availableModels.length > 0 && (
              <div className="space-y-1.5">
                <Label htmlFor="ai-model">Model</Label>
                <Select
                  value={aiModel === '' ? '__default__' : aiModel}
                  onValueChange={(v) => setAiModel(v === '__default__' ? '' : v)}
                >
                  <SelectTrigger id="ai-model">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="__default__">Use provider default</SelectItem>
                    {availableModels.map((m) => (
                      <SelectItem key={m} value={m}>{m}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  Overrides the provider's default model. Passed as <code className="bg-muted px-1 rounded">--model</code> to the CLI.
                </p>
              </div>
            )}
            <div className="space-y-1.5">
              <Label htmlFor="prompt">Custom prompt</Label>
              <Textarea
                id="prompt"
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                placeholder="Leave empty to use the default prompt for the trigger type"
                rows={3}
              />
            </div>
          </div>

          {/* Limits */}
          <div className="space-y-3">
            <h3 className="text-sm font-medium">Limits</h3>
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-1.5">
                <Label htmlFor="max-turns">Max turns</Label>
                <Input
                  id="max-turns"
                  type="number"
                  value={maxTurns}
                  onChange={(e) => setMaxTurns(parseInt(e.target.value) || 0)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="timeout">Timeout (sec)</Label>
                <Input
                  id="timeout"
                  type="number"
                  value={timeoutSeconds}
                  onChange={(e) =>
                    setTimeoutSeconds(parseInt(e.target.value) || 0)
                  }
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="budget">Daily budget (cents)</Label>
                <Input
                  id="budget"
                  type="number"
                  value={dailyBudgetCents}
                  onChange={(e) =>
                    setDailyBudgetCents(parseInt(e.target.value) || 0)
                  }
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="cooldown">Cooldown (min)</Label>
                <Input
                  id="cooldown"
                  type="number"
                  value={cooldownMinutes}
                  onChange={(e) =>
                    setCooldownMinutes(parseInt(e.target.value) || 0)
                  }
                />
              </div>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="branch-prefix">Branch prefix</Label>
              <Input
                id="branch-prefix"
                value={branchPrefix}
                onChange={(e) => setBranchPrefix(e.target.value)}
                placeholder="agents/"
              />
            </div>
          </div>

          {/* Config Repo */}
          <div className="space-y-3">
            <h3 className="text-sm font-medium">Config Repository</h3>
            <p className="text-xs text-muted-foreground">
              A private repository containing a <code className="bg-muted px-1 rounded">.claude/</code> directory
              with skills, MCP servers, and settings. Overlaid into the sandbox at runtime.
            </p>
            <div className="space-y-1.5">
              <Label htmlFor="config-repo-url">Repository</Label>
              <Input
                id="config-repo-url"
                value={configRepoUrl}
                onChange={(e) => setConfigRepoUrl(e.target.value)}
                placeholder="org/my-claude-config"
              />
              <p className="text-xs text-muted-foreground">
                GitHub repo path (e.g. <code className="bg-muted px-1 rounded">org/claude-skills</code>).
                Leave empty to use global config only.
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="config-repo-branch">Branch</Label>
              <Input
                id="config-repo-branch"
                value={configRepoBranch}
                onChange={(e) => setConfigRepoBranch(e.target.value)}
                placeholder="main"
              />
            </div>
          </div>

          {/* Skills */}
          <div className="space-y-3">
            <h3 className="text-sm font-medium">Skills</h3>
            <p className="text-xs text-muted-foreground">
              Select project-level skills to make available to this workflow.
              Skills are injected as <code className="bg-muted px-1 rounded">.claude/skills/</code> files in the sandbox.
            </p>
            {availableSkills.length > 0 ? (
              <div className="space-y-2">
                {availableSkills.map((skill) => (
                  <label
                    key={skill.slug}
                    className="flex items-start gap-3 rounded-md border p-3 cursor-pointer hover:bg-muted/50"
                  >
                    <Checkbox
                      checked={selectedSkills.includes(skill.slug)}
                      onCheckedChange={() => toggleSkill(skill.slug)}
                      className="mt-0.5"
                    />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-1.5">
                        <span className="text-sm font-medium">{skill.name}</span>
                        {skill.project_id === null && (
                          <span className="text-[10px] px-1 py-0.5 rounded bg-muted text-muted-foreground">Global</span>
                        )}
                      </div>
                      <div className="text-xs text-muted-foreground truncate">
                        {skill.description || skill.slug}
                      </div>
                    </div>
                  </label>
                ))}
              </div>
            ) : (
              <p className="text-xs text-muted-foreground italic">
                No skills defined. Create skills in project or platform settings.
              </p>
            )}
          </div>

          {/* MCP Servers */}
          <div className="space-y-3">
            <h3 className="text-sm font-medium">MCP Servers</h3>
            <p className="text-xs text-muted-foreground">
              Select MCP servers to make available to this workflow.
              Configs are merged into <code className="bg-muted px-1 rounded">.claude/settings.json</code> at runtime.
            </p>
            {availableMcps.length > 0 ? (
              <div className="space-y-2">
                {availableMcps.map((mcp) => (
                  <label
                    key={mcp.slug}
                    className="flex items-start gap-3 rounded-md border p-3 cursor-pointer hover:bg-muted/50"
                  >
                    <Checkbox
                      checked={selectedMcps.includes(mcp.slug)}
                      onCheckedChange={() => toggleMcp(mcp.slug)}
                      className="mt-0.5"
                    />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-1.5">
                        <span className="text-sm font-medium">{mcp.name}</span>
                        {mcp.project_id === null && (
                          <span className="text-[10px] px-1 py-0.5 rounded bg-muted text-muted-foreground">Global</span>
                        )}
                      </div>
                      <div className="text-xs text-muted-foreground truncate">
                        {mcp.description || mcp.slug}
                      </div>
                    </div>
                  </label>
                ))}
              </div>
            ) : (
              <p className="text-xs text-muted-foreground italic">
                No MCP servers defined. Create MCP servers in project or platform settings.
              </p>
            )}
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={isPending}>
              {isPending
                ? isEdit
                  ? 'Saving...'
                  : 'Creating...'
                : isEdit
                  ? 'Save'
                  : 'Create Workflow'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
