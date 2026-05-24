import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import {
  getAgentOptions,
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

export type Agent = Omit<
  AgentConfigResponse,
  'trigger_config' | 'skills_config' | 'mcp_servers_config'
> & {
  trigger_config?: TriggerConfig | null
  skills_config?: string[] | null
  mcp_servers_config?: string[] | null
}

interface AgentEditPageProps {
  project: { id: number; slug: string }
}

export function AgentEditPage({ project }: AgentEditPageProps) {
  const { agentSlug } = useParams<{ agentSlug: string }>()
  const backHref = `/projects/${project.slug}/agents/detail/${agentSlug ?? ''}`

  const { data: agentRaw, isLoading, error } = useQuery({
    ...getAgentOptions({
      path: { project_id: project.id, slug: agentSlug! },
    }),
    enabled: !!agentSlug,
  })
  const agent = agentRaw as Agent | undefined

  if (isLoading) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-96 w-full" />
      </div>
    )
  }

  if (error || !agent) {
    return (
      <div className="flex flex-col items-center justify-center py-20">
        <p className="text-muted-foreground text-sm">Workflow not found</p>
        <Button variant="ghost" size="sm" className="mt-4" asChild>
          <Link to={`/projects/${project.slug}/agents`}>Back to workflows</Link>
        </Button>
      </div>
    )
  }

  // Inner form mounts only once `agent` is loaded. Mounting after the data
  // is available lets useState initialisers see the real values — Radix
  // Select shows nothing if it mounts with `value=""` and then later
  // transitions to a real value, which is what caused the empty Provider
  // and Deliverable fields. The key={agent.id} also forces a fresh mount
  // if the user navigates from one workflow to another.
  return (
    <AgentEditForm
      key={agent.id}
      project={project}
      agent={agent}
      backHref={backHref}
    />
  )
}

function AgentEditForm({
  project,
  agent,
  backHref,
}: {
  project: { id: number; slug: string }
  agent: Agent
  backHref: string
}) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  // Lazy initialisers seed every field from `agent` on first mount, so the
  // Radix Select components never see an empty `value` followed by a real
  // one. Defaults match AgentSettingsDialog — new-issue / regression /
  // manual stay `false` for workflows whose YAML omits the trigger.
  const [name, setName] = useState(agent.name ?? '')
  const [description, setDescription] = useState(agent.description ?? '')
  const [enabled, setEnabled] = useState(agent.enabled ?? false)
  const [aiProvider, setAiProvider] = useState(agent.ai_provider ?? '')
  const [aiModel, setAiModel] = useState<string>(agent.ai_model ?? '')
  const [prompt, setPrompt] = useState(agent.prompt ?? '')
  const [maxTurns, setMaxTurns] = useState(agent.max_turns ?? 25)
  const [timeoutSeconds, setTimeoutSeconds] = useState(agent.timeout_seconds ?? 600)
  const [dailyBudgetCents, setDailyBudgetCents] = useState(agent.daily_budget_cents ?? 500)
  const [cooldownMinutes, setCooldownMinutes] = useState(agent.cooldown_minutes ?? 30)
  const [branchPrefix, setBranchPrefix] = useState(agent.branch_prefix ?? 'agents/')
  const [deliverable, setDeliverable] = useState(agent.deliverable ?? 'pull_request')
  const [configRepoUrl, setConfigRepoUrl] = useState(agent.config_repo_url ?? '')
  const [configRepoBranch, setConfigRepoBranch] = useState(agent.config_repo_branch ?? '')
  const [selectedSkills, setSelectedSkills] = useState<string[]>(agent.skills_config ?? [])
  const [selectedMcps, setSelectedMcps] = useState<string[]>(agent.mcp_servers_config ?? [])
  const [triggerNewIssue, setTriggerNewIssue] = useState(
    agent.trigger_config?.error?.new_issue ?? false,
  )
  const [triggerRegression, setTriggerRegression] = useState(
    agent.trigger_config?.error?.regression ?? false,
  )
  const [triggerManual, setTriggerManual] = useState(agent.trigger_config?.manual ?? false)
  const [triggerCron, setTriggerCron] = useState(agent.trigger_config?.schedule?.cron ?? '')

  const { data: providerCatalog } = useQuery({
    queryKey: ['ai-provider-catalog'],
    queryFn: async () => {
      const res = await fetch('/api/settings/ai-providers')
      if (!res.ok) return null
      return res.json() as Promise<{
        default_provider: string
        providers: Array<{
          id: string
          models: string[]
          default_model: string | null
        }>
      }>
    },
    staleTime: 60 * 1000,
  })
  const availableModels =
    providerCatalog?.providers.find((p) => p.id === aiProvider)?.models ?? []

  const { data: projectSkillsData } = useQuery({
    ...listSkillsOptions({ path: { project_id: project.id } }),
  })
  const { data: globalSkillsData } = useQuery({ ...listGlobalSkillsOptions() })
  const projectSkills = projectSkillsData?.items ?? []
  const globalSkills = globalSkillsData?.items ?? []
  const availableSkills = [
    ...projectSkills,
    ...globalSkills.filter(
      (g) => !projectSkills.some((p) => p.slug === g.slug),
    ),
  ]

  const { data: projectMcpsData } = useQuery({
    ...listMcpsOptions({ path: { project_id: project.id } }),
  })
  const { data: globalMcpsData } = useQuery({ ...listGlobalMcpsOptions() })
  const projectMcps = projectMcpsData?.items ?? []
  const globalMcps = globalMcpsData?.items ?? []
  const availableMcps = [
    ...projectMcps,
    ...globalMcps.filter((g) => !projectMcps.some((p) => p.slug === g.slug)),
  ]

  const toggleSkill = (slug: string) =>
    setSelectedSkills((prev) =>
      prev.includes(slug) ? prev.filter((s) => s !== slug) : [...prev, slug],
    )
  const toggleMcp = (slug: string) =>
    setSelectedMcps((prev) =>
      prev.includes(slug) ? prev.filter((s) => s !== slug) : [...prev, slug],
    )

  const updateMutation = useMutation({
    ...updateAgentMutation(),
    onSuccess: () => {
      toast.success('Workflow updated')
      queryClient.invalidateQueries({
        queryKey: listAgentsQueryKey({ path: { project_id: project.id } }),
      })
      navigate(backHref)
    },
    onError: (err: Error) => {
      toast.error(err.message || 'Failed to update workflow')
    },
  })

  const handleSubmit = () => {
    if (!agent) return
    // Only include trigger blocks the user actually enabled. Writing every
    // trigger unconditionally caused workflows to persist triggers they
    // never asked for.
    const triggerConfig: Record<string, unknown> = {}
    if (triggerNewIssue || triggerRegression) {
      triggerConfig.error = {
        new_issue: triggerNewIssue,
        regression: triggerRegression,
      }
    }
    if (triggerManual) triggerConfig.manual = true
    if (triggerCron.trim())
      triggerConfig.schedule = { cron: triggerCron.trim() }

    const body: UpsertAgentRequest = {
      slug: agent.slug,
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
      path: { project_id: project.id, slug: agent.slug },
      body,
    })
  }

  const isSubmitting = updateMutation.isPending
  const isYaml = agent.source === 'yaml'

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault()
        handleSubmit()
      }}
      className="mx-auto max-w-3xl space-y-8 pb-12"
    >
      {/* Sticky header with primary actions */}
      <div className="sticky top-0 z-10 -mx-4 flex items-center justify-between gap-4 border-b bg-background/95 px-4 py-3 backdrop-blur sm:-mx-6 sm:px-6">
        <div className="flex min-w-0 items-center gap-3">
          <Button variant="ghost" size="icon" asChild>
            <Link to={backHref} aria-label="Back to workflow">
              <ArrowLeft className="h-4 w-4" />
            </Link>
          </Button>
          <div className="min-w-0">
            <p className="text-muted-foreground truncate text-xs">
              Edit workflow
            </p>
            <h1 className="truncate text-base font-semibold">{agent.name}</h1>
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => navigate(backHref)}
            disabled={isSubmitting}
          >
            Cancel
          </Button>
          <Button type="submit" size="sm" disabled={isSubmitting}>
            {isSubmitting ? 'Saving…' : 'Save'}
          </Button>
        </div>
      </div>

      {isYaml && (
        <div className="rounded-md border border-amber-500/30 bg-amber-500/5 p-3">
          <p className="text-sm">
            This workflow is managed by{' '}
            <code className="bg-muted rounded px-1">
              .temps/agents/{agent.slug}.yaml
            </code>
            . Saving here updates the live config, but the next deploy will
            sync the YAML file back and overwrite your changes.
          </p>
        </div>
      )}

      {/* Basic info */}
      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">Workflow</h2>
          <p className="text-muted-foreground mt-1 text-sm">
            Name, description, and on/off state.
          </p>
        </div>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="name">Name</Label>
            <Input
              id="name"
              name="name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Error Fixer"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="description">Description</Label>
            <Input
              id="description"
              name="description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="What this workflow does"
            />
          </div>
          <div className="flex items-center justify-between rounded-md border p-3">
            <div className="space-y-0.5">
              <Label htmlFor="enabled" className="text-sm">
                Enabled
              </Label>
              <p className="text-muted-foreground text-xs">
                When off, the workflow won't run on any trigger.
              </p>
            </div>
            <Switch
              id="enabled"
              checked={enabled}
              onCheckedChange={setEnabled}
            />
          </div>
        </div>
      </section>

      <Separator />

      {/* Deliverable */}
      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">Deliverable</h2>
          <p className="text-muted-foreground mt-1 text-sm">
            What this workflow produces when it runs.
          </p>
        </div>
        <div className="space-y-2">
          <Select value={deliverable} onValueChange={setDeliverable}>
            <SelectTrigger aria-label="Deliverable">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="pull_request">Pull Request</SelectItem>
              <SelectItem value="report">Report</SelectItem>
            </SelectContent>
          </Select>
          <p className="text-muted-foreground text-xs">
            {deliverable === 'report'
              ? 'Produces a report — no branch, PR, or deployment.'
              : 'Pushes a branch, opens a PR, and triggers a preview deployment.'}
          </p>
        </div>
      </section>

      <Separator />

      {/* Triggers */}
      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">Triggers</h2>
          <p className="text-muted-foreground mt-1 text-sm">
            When this workflow runs. Disabled triggers aren't saved to the
            workflow config.
          </p>
        </div>
        <div className="divide-y rounded-md border">
          <div className="flex items-center justify-between p-3">
            <Label htmlFor="trigger-new-issue" className="text-sm font-normal">
              New errors
            </Label>
            <Switch
              id="trigger-new-issue"
              checked={triggerNewIssue}
              onCheckedChange={setTriggerNewIssue}
            />
          </div>
          <div className="flex items-center justify-between p-3">
            <Label htmlFor="trigger-regression" className="text-sm font-normal">
              Regressions
            </Label>
            <Switch
              id="trigger-regression"
              checked={triggerRegression}
              onCheckedChange={setTriggerRegression}
            />
          </div>
          <div className="flex items-center justify-between p-3">
            <Label htmlFor="trigger-manual" className="text-sm font-normal">
              Manual trigger
            </Label>
            <Switch
              id="trigger-manual"
              checked={triggerManual}
              onCheckedChange={setTriggerManual}
            />
          </div>
          <div className="space-y-1.5 p-3">
            <Label htmlFor="trigger-cron" className="text-sm font-normal">
              Schedule (cron)
            </Label>
            <Input
              id="trigger-cron"
              name="trigger-cron"
              placeholder="e.g. 0 * * * * (every hour)"
              value={triggerCron}
              onChange={(e) => setTriggerCron(e.target.value)}
            />
            <p className="text-muted-foreground text-xs">
              Leave empty for no schedule. Standard 5-field cron syntax.
            </p>
          </div>
        </div>
      </section>

      <Separator />

      {/* AI Provider */}
      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">AI provider</h2>
          <p className="text-muted-foreground mt-1 text-sm">
            Which CLI runs the workflow.
          </p>
        </div>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <div className="flex items-center justify-between gap-2">
              <Label htmlFor="ai-provider">Provider</Label>
              {providerCatalog &&
                aiProvider === providerCatalog.default_provider && (
                  <span className="text-muted-foreground bg-muted rounded px-1.5 py-0.5 text-[10px]">
                    Platform default
                  </span>
                )}
            </div>
            <Select
              value={aiProvider}
              onValueChange={(v) => {
                setAiProvider(v)
                setAiModel('')
              }}
            >
              <SelectTrigger id="ai-provider">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="claude_cli">Claude Code</SelectItem>
                <SelectItem value="opencode">OpenCode</SelectItem>
                <SelectItem value="codex_cli">Codex</SelectItem>
              </SelectContent>
            </Select>
            {providerCatalog &&
              aiProvider === providerCatalog.default_provider && (
                <p className="text-muted-foreground text-xs">
                  Inherits the platform default. Change in{' '}
                  <Link
                    to="/agent-sandbox/providers"
                    className="hover:text-foreground underline underline-offset-2"
                  >
                    Agent Sandbox &rarr; Providers
                  </Link>
                  .
                </p>
              )}
          </div>
          {availableModels.length > 0 && (
            <div className="space-y-1.5">
              <Label htmlFor="ai-model">Model</Label>
              <Select
                value={aiModel === '' ? '__default__' : aiModel}
                onValueChange={(v) =>
                  setAiModel(v === '__default__' ? '' : v)
                }
              >
                <SelectTrigger id="ai-model">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="__default__">
                    Use provider default
                  </SelectItem>
                  {availableModels.map((m) => (
                    <SelectItem key={m} value={m}>
                      {m}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-muted-foreground text-xs">
                Overrides the provider's default model. Passed as{' '}
                <code className="bg-muted rounded px-1">--model</code> to the
                CLI.
              </p>
            </div>
          )}
          <div className="space-y-1.5">
            <Label htmlFor="prompt">Custom prompt</Label>
            <Textarea
              id="prompt"
              name="prompt"
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="Leave empty to use the default prompt for the trigger type"
              rows={5}
            />
          </div>
        </div>
      </section>

      <Separator />

      {/* Config repo */}
      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">Config repository</h2>
          <p className="text-muted-foreground mt-1 text-sm">
            Private repo with a <code className="bg-muted rounded px-1">.claude/</code>{' '}
            directory containing skills, MCP servers, and settings. Overlaid
            into the sandbox at runtime.
          </p>
        </div>
        <div className="grid gap-4 sm:grid-cols-[2fr_1fr]">
          <div className="space-y-1.5">
            <Label htmlFor="config-repo-url">Repository</Label>
            <Input
              id="config-repo-url"
              name="config-repo-url"
              value={configRepoUrl}
              onChange={(e) => setConfigRepoUrl(e.target.value)}
              placeholder="org/my-claude-config"
            />
            <p className="text-muted-foreground text-xs">
              GitHub repo path. Leave empty to use global config only.
            </p>
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="config-repo-branch">Branch</Label>
            <Input
              id="config-repo-branch"
              name="config-repo-branch"
              value={configRepoBranch}
              onChange={(e) => setConfigRepoBranch(e.target.value)}
              placeholder="main"
            />
          </div>
        </div>
      </section>

      <Separator />

      {/* Skills */}
      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">Skills</h2>
          <p className="text-muted-foreground mt-1 text-sm">
            Project- and platform-level skills available to this workflow.
            Injected as{' '}
            <code className="bg-muted rounded px-1">.claude/skills/</code> files
            in the sandbox.
          </p>
        </div>
        {availableSkills.length > 0 ? (
          <div className="space-y-2" role="list">
            {availableSkills.map((skill) => {
              const checked = selectedSkills.includes(skill.slug)
              return (
                <label
                  key={skill.slug}
                  className="hover:bg-muted/50 has-data-state-checked:border-foreground/30 has-data-state-checked:bg-muted/30 flex cursor-pointer items-start gap-3 rounded-md border p-3 transition-colors"
                >
                  <Checkbox
                    checked={checked}
                    onCheckedChange={() => toggleSkill(skill.slug)}
                    className="mt-0.5"
                    aria-label={skill.name}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <span className="text-sm font-medium">{skill.name}</span>
                      {skill.project_id === null && (
                        <span className="bg-muted text-muted-foreground rounded px-1 py-0.5 text-[10px]">
                          Global
                        </span>
                      )}
                    </div>
                    <p className="text-muted-foreground mt-0.5 truncate text-xs">
                      {skill.description || skill.slug}
                    </p>
                  </div>
                </label>
              )
            })}
          </div>
        ) : (
          <div className="rounded-md border border-dashed p-6 text-center">
            <p className="text-muted-foreground text-sm">
              No skills defined.
            </p>
            <p className="text-muted-foreground mt-1 text-xs">
              Create skills in project or platform settings.
            </p>
          </div>
        )}
      </section>

      <Separator />

      {/* MCP servers */}
      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">MCP servers</h2>
          <p className="text-muted-foreground mt-1 text-sm">
            MCP servers available to this workflow. Configs are merged into{' '}
            <code className="bg-muted rounded px-1">.claude/settings.json</code>{' '}
            at runtime.
          </p>
        </div>
        {availableMcps.length > 0 ? (
          <div className="space-y-2" role="list">
            {availableMcps.map((mcp) => {
              const checked = selectedMcps.includes(mcp.slug)
              return (
                <label
                  key={mcp.slug}
                  className="hover:bg-muted/50 has-data-state-checked:border-foreground/30 has-data-state-checked:bg-muted/30 flex cursor-pointer items-start gap-3 rounded-md border p-3 transition-colors"
                >
                  <Checkbox
                    checked={checked}
                    onCheckedChange={() => toggleMcp(mcp.slug)}
                    className="mt-0.5"
                    aria-label={mcp.name}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <span className="text-sm font-medium">{mcp.name}</span>
                      {mcp.project_id === null && (
                        <span className="bg-muted text-muted-foreground rounded px-1 py-0.5 text-[10px]">
                          Global
                        </span>
                      )}
                    </div>
                    <p className="text-muted-foreground mt-0.5 truncate text-xs">
                      {mcp.description || mcp.slug}
                    </p>
                  </div>
                </label>
              )
            })}
          </div>
        ) : (
          <div className="rounded-md border border-dashed p-6 text-center">
            <p className="text-muted-foreground text-sm">
              No MCP servers defined.
            </p>
            <p className="text-muted-foreground mt-1 text-xs">
              Create MCP servers in project or platform settings.
            </p>
          </div>
        )}
      </section>

      <Separator />

      {/* Limits & sandbox */}
      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">Limits</h2>
          <p className="text-muted-foreground mt-1 text-sm">
            Guard rails so a stuck workflow can't burn the budget.
          </p>
        </div>
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="max-turns">Max turns</Label>
            <Input
              id="max-turns"
              name="max-turns"
              type="number"
              min={0}
              className="tabular-nums"
              value={maxTurns}
              onChange={(e) => setMaxTurns(parseInt(e.target.value) || 0)}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="timeout">Timeout (sec)</Label>
            <Input
              id="timeout"
              name="timeout"
              type="number"
              min={0}
              className="tabular-nums"
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
              name="budget"
              type="number"
              min={0}
              className="tabular-nums"
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
              name="cooldown"
              type="number"
              min={0}
              className="tabular-nums"
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
            name="branch-prefix"
            value={branchPrefix}
            onChange={(e) => setBranchPrefix(e.target.value)}
            placeholder="agents/"
          />
          <p className="text-muted-foreground text-xs">
            Prefix for branches created by Pull Request workflows.
          </p>
        </div>
      </section>

      {/* Sticky bottom action row mirrors the header so long forms don't
          force scrolling back up to save. */}
      <div className="border-t pt-4">
        <div className="flex items-center justify-end gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => navigate(backHref)}
            disabled={isSubmitting}
          >
            Cancel
          </Button>
          <Button type="submit" size="sm" disabled={isSubmitting}>
            {isSubmitting ? 'Saving…' : 'Save'}
          </Button>
        </div>
      </div>
    </form>
  )
}
