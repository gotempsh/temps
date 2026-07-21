import { useCallback, useEffect, useState } from 'react'
import { toast } from 'sonner'
import {
  AlertTriangle,
  Bot,
  CheckCircle2,
  Cpu,
  Loader2,
  RefreshCw,
  Save,
  XCircle,
} from 'lucide-react'

import { usePageTitle } from '@/hooks/usePageTitle'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'

interface SandboxStatus {
  docker_available: boolean
  image_ready: boolean
  image_name: string
  error: string | null
}

const RUNTIME_PRESETS = [
  { value: 'node', label: 'Node.js', description: 'Node.js 20, npm, npx', stacks: 'Next.js, Vite, Express, any JS/TS' },
  { value: 'bun', label: 'Bun', description: 'Bun runtime', stacks: 'Bun-based projects' },
  { value: 'python', label: 'Python', description: 'Python 3.12, pip, uv', stacks: 'Django, FastAPI, Flask' },
  { value: 'rust', label: 'Rust', description: 'Rust stable, cargo', stacks: 'Rust projects' },
  { value: 'go', label: 'Go', description: 'Go 1.23', stacks: 'Go projects' },
  { value: 'full', label: 'Full', description: 'Node, Python, Go, uv', stacks: 'Multi-language projects' },
  { value: 'custom', label: 'Custom Image', description: 'Your own Docker image', stacks: 'Any stack you pre-build' },
]

const RESOURCE_PRESETS = [
  { label: 'Light', cpu: 2, memory: 4096 },
  { label: 'Standard', cpu: 4, memory: 8192 },
  { label: 'Heavy', cpu: 8, memory: 16384 },
  { label: 'Custom', cpu: 0, memory: 0 },
]

function getResourcePresetLabel(cpu: number, memory: number): string {
  const match = RESOURCE_PRESETS.find((p) => p.cpu === cpu && p.memory === memory)
  return match ? match.label : 'Custom'
}

export function AgentSandboxSandboxPage() {
  usePageTitle('Sandbox Configuration')
  const { data: settings, isLoading } = useSettings()
  const updateSettings = useUpdateSettings()

  const [runtime, setRuntime] = useState('node')
  const [customImage, setCustomImage] = useState('')
  const [cpuLimit, setCpuLimit] = useState(4)
  const [memoryLimitMb, setMemoryLimitMb] = useState(8192)
  const [resourcePreset, setResourcePreset] = useState('Standard')
  const [globalConfigRepo, setGlobalConfigRepo] = useState('')
  const [globalConfigRepoBranch, setGlobalConfigRepoBranch] = useState('main')
  const [defaultProvider, setDefaultProvider] = useState('claude_cli')
  const [isDirty, setIsDirty] = useState(false)

  const [sandboxStatus, setSandboxStatus] = useState<SandboxStatus | null>(null)
  const [statusLoading, setStatusLoading] = useState(false)
  const [rebuilding, setRebuilding] = useState(false)
  const [buildLog, setBuildLog] = useState<string[]>([])

  const fetchSandboxStatus = useCallback(async () => {
    setStatusLoading(true)
    try {
      // TODO(sdk-regen): migrate once /settings/sandbox-status endpoint is
      // added to the generated SDK.
      const r = await fetch('/api/settings/sandbox-status')
      if (r.ok) setSandboxStatus(await r.json())
    } catch {
      // tolerate older versions without the endpoint
    } finally {
      setStatusLoading(false)
    }
  }, [])

  useEffect(() => {
    if (settings?.agent_sandbox) {
      const s = settings.agent_sandbox
      setDefaultProvider(s.default_provider || 'claude_cli')
      setRuntime(s.runtime || 'node')
      setCustomImage(s.custom_image || '')
      setCpuLimit(s.cpu_limit)
      setMemoryLimitMb(s.memory_limit_mb)
      setResourcePreset(getResourcePresetLabel(s.cpu_limit, s.memory_limit_mb))
    }
    if (settings?.ai_config) {
      setGlobalConfigRepo(settings.ai_config.config_repo || '')
      setGlobalConfigRepoBranch(settings.ai_config.config_repo_branch || 'main')
    }
  }, [settings])

  useEffect(() => {
    fetchSandboxStatus()
  }, [fetchSandboxStatus])

  const handleRebuildImage = async () => {
    setRebuilding(true)
    setBuildLog([])
    try {
      // TODO(sdk-regen): migrate once /settings/sandbox-rebuild (streaming SSE)
      // endpoint is added to the generated SDK.
      const response = await fetch('/api/settings/sandbox-rebuild', { method: 'POST' })
      if (!response.ok || !response.body) {
        toast.error('Failed to start image rebuild')
        setRebuilding(false)
        return
      }
      const reader = response.body.getReader()
      const decoder = new TextDecoder()
      let buffer = ''

      while (true) {
        const { done, value } = await reader.read()
        if (done) break
        buffer += decoder.decode(value, { stream: true })
        const lines = buffer.split('\n')
        buffer = lines.pop() || ''
        for (const line of lines) {
          if (line.startsWith('data:')) {
            const data = line.slice(5).trim()
            if (!data) continue
            try {
              const parsed = JSON.parse(data)
              if (parsed.type === 'done') {
                if (parsed.success) toast.success(`Image rebuilt: ${parsed.image_name}`)
                else toast.error(parsed.error || 'Build failed')
                continue
              }
            } catch {
              // not JSON — treat as a log line
            }
            setBuildLog((prev) => [...prev, data])
          }
        }
      }
      await fetchSandboxStatus()
    } catch {
      toast.error('Failed to rebuild image')
    } finally {
      setRebuilding(false)
    }
  }

  const handleResourcePresetChange = (preset: string) => {
    setResourcePreset(preset)
    const p = RESOURCE_PRESETS.find((r) => r.label === preset)
    if (p && p.cpu > 0) {
      setCpuLimit(p.cpu)
      setMemoryLimitMb(p.memory)
    }
    setIsDirty(true)
  }

  const handleSave = async () => {
    try {
      // Per-provider credentials live on /agent-sandbox/providers/:id —
      // this mutation only touches platform-wide sandbox knobs. We pass
      // through default_provider so we don't accidentally clobber it on
      // save (it's also editable from the providers list).
      await updateSettings.mutateAsync({
        agent_sandbox: {
          // Carry forward fields we don't edit on this page (providers map,
          // legacy auth_type / api_key_encrypted) so a save here doesn't wipe
          // per-provider credentials configured on /agent-sandbox/providers/:id.
          ...settings?.agent_sandbox,
          default_provider: defaultProvider,
          enabled: true,
          runtime,
          custom_image: customImage,
          cpu_limit: cpuLimit,
          memory_limit_mb: memoryLimitMb,
          network_mode: settings?.agent_sandbox?.network_mode ?? 'full',
          api_key_saved: settings?.agent_sandbox?.api_key_saved ?? false,
          auth_type: settings?.agent_sandbox?.auth_type ?? '',
          providers: settings?.agent_sandbox?.providers ?? {},
        },
        ai_config: {
          ...(settings?.ai_config ?? {}),
          config_repo: globalConfigRepo,
          config_repo_branch: globalConfigRepoBranch,
        },
      })
      setIsDirty(false)
      toast.success('Workflow sandbox settings saved')
    } catch {
      toast.error('Failed to save settings')
    }
  }

  if (isLoading) {
    return (
      <div className="flex justify-center py-12">
        <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
      </div>
    )
  }

  return (
    <>
      {/* Bottom padding leaves room for the sticky save bar so its drop
          shadow never overlaps the last card. */}
      <div className="space-y-6 pb-24">
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <Bot className="h-5 w-5" />
              Workflow Sandbox
            </CardTitle>
            <CardDescription>
              When sandbox is enabled, workflows run inside isolated Docker containers.
              Code changes are contained and can't affect your server.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            <div className="rounded-lg border p-4 space-y-3">
              <div className="flex items-center justify-between">
                <h4 className="text-sm font-medium">Docker Status</h4>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={fetchSandboxStatus}
                  disabled={statusLoading}
                >
                  {statusLoading ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <RefreshCw className="h-3.5 w-3.5" />
                  )}
                </Button>
              </div>
              {sandboxStatus ? (
                <div className="space-y-2">
                  <div className="flex items-center gap-2 text-sm">
                    {sandboxStatus.docker_available ? (
                      <CheckCircle2 className="h-4 w-4 text-green-500" />
                    ) : (
                      <XCircle className="h-4 w-4 text-red-500" />
                    )}
                    <span>
                      Docker:{' '}
                      {sandboxStatus.docker_available ? 'connected' : 'not available'}
                    </span>
                  </div>
                  {sandboxStatus.docker_available && (
                    <div className="flex items-center gap-2 text-sm">
                      {sandboxStatus.image_ready ? (
                        <CheckCircle2 className="h-4 w-4 text-green-500" />
                      ) : (
                        <XCircle className="h-4 w-4 text-amber-500" />
                      )}
                      <span>
                        Image:{' '}
                        {sandboxStatus.image_ready
                          ? sandboxStatus.image_name
                          : 'not built (will build automatically on first run)'}
                      </span>
                    </div>
                  )}
                  {sandboxStatus.error && (
                    <p className="text-xs text-red-400 mt-1">{sandboxStatus.error}</p>
                  )}
                  {sandboxStatus.docker_available && (
                    <div className="space-y-2 mt-2">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={handleRebuildImage}
                        disabled={rebuilding}
                      >
                        {rebuilding ? (
                          <Loader2 className="h-3.5 w-3.5 animate-spin mr-2" />
                        ) : (
                          <RefreshCw className="h-3.5 w-3.5 mr-2" />
                        )}
                        {rebuilding ? 'Rebuilding...' : 'Rebuild Image'}
                      </Button>
                      {buildLog.length > 0 && (
                        <div className="max-h-60 overflow-y-auto rounded border bg-black/80 p-3 font-mono text-xs text-green-400 space-y-0.5">
                          {buildLog.map((line, i) => (
                            <div key={i}>{line}</div>
                          ))}
                          {rebuilding && (
                            <div className="animate-pulse text-green-300">...</div>
                          )}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              ) : statusLoading ? (
                <p className="text-sm text-muted-foreground">Checking...</p>
              ) : null}
            </div>

            <Alert>
              <AlertTriangle className="h-4 w-4" />
              <AlertDescription>
                Agents always run in an isolated Docker container — there is no
                host-execution mode. Docker must be available for any session
                to start.
              </AlertDescription>
            </Alert>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">Runtime</CardTitle>
            <CardDescription>
              Choose the runtime environment for sandbox containers. Each preset
              includes the language toolchain, git, and Claude CLI pre-installed.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 gap-2">
              {RUNTIME_PRESETS.map((preset) => (
                <button
                  key={preset.value}
                  onClick={() => {
                    setRuntime(preset.value)
                    setIsDirty(true)
                  }}
                  className={`rounded-lg border p-3 text-left transition-colors ${
                    runtime === preset.value
                      ? 'border-primary bg-primary/5'
                      : 'border-border hover:border-primary/50'
                  }`}
                >
                  <p className="text-sm font-medium">{preset.label}</p>
                  <p className="text-xs text-muted-foreground">{preset.description}</p>
                </button>
              ))}
            </div>

            {runtime === 'custom' && (
              <div className="space-y-2 pt-2">
                <Label htmlFor="custom-image">Docker Image</Label>
                <Input
                  id="custom-image"
                  placeholder="e.g. your-registry/custom-agent:latest"
                  value={customImage}
                  onChange={(e) => {
                    setCustomImage(e.target.value)
                    setIsDirty(true)
                  }}
                />
                <p className="text-xs text-muted-foreground">
                  Custom images must have <code className="text-xs bg-muted px-1 rounded">git</code>{' '}
                  and <code className="text-xs bg-muted px-1 rounded">claude</code> (Claude CLI)
                  installed. The repository is mounted at{' '}
                  <code className="text-xs bg-muted px-1 rounded">/workspace</code>.
                </p>
              </div>
            )}

            {runtime !== 'custom' && (
              <p className="text-xs text-muted-foreground">
                {RUNTIME_PRESETS.find((p) => p.value === runtime)?.stacks}
              </p>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <Cpu className="h-4 w-4" />
              Resources
            </CardTitle>
            <CardDescription>
              CPU and memory limits per sandbox container.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="space-y-2">
              <Label>Preset</Label>
              <div className="grid grid-cols-2 sm:grid-cols-4 gap-2">
                {RESOURCE_PRESETS.map((preset) => (
                  <button
                    key={preset.label}
                    onClick={() => handleResourcePresetChange(preset.label)}
                    className={`rounded-lg border p-3 text-left transition-colors ${
                      resourcePreset === preset.label
                        ? 'border-primary bg-primary/5'
                        : 'border-border hover:border-primary/50'
                    }`}
                  >
                    <p className="text-sm font-medium">{preset.label}</p>
                    <p className="text-xs text-muted-foreground">
                      {preset.label === 'Custom'
                        ? 'Set your own'
                        : `${preset.cpu} CPU, ${preset.memory >= 1024 ? `${preset.memory / 1024}GB` : `${preset.memory}MB`}`}
                    </p>
                  </button>
                ))}
              </div>
            </div>

            {resourcePreset === 'Custom' && (
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-4 pt-2">
                <div className="space-y-2">
                  <Label htmlFor="cpu-limit">CPU cores</Label>
                  <Input
                    id="cpu-limit"
                    type="number"
                    min={0.5}
                    max={16}
                    step={0.5}
                    value={cpuLimit}
                    onChange={(e) => {
                      setCpuLimit(parseFloat(e.target.value) || 2)
                      setIsDirty(true)
                    }}
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="memory-limit">Memory (MB)</Label>
                  <Input
                    id="memory-limit"
                    type="number"
                    min={256}
                    max={32768}
                    step={256}
                    value={memoryLimitMb}
                    onChange={(e) => {
                      setMemoryLimitMb(parseInt(e.target.value) || 8192)
                      setIsDirty(true)
                    }}
                  />
                </div>
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">Global Config Repository</CardTitle>
            <CardDescription>
              A shared config repository applied to all workflow runs. Contains a{' '}
              <code className="text-xs bg-muted px-1 rounded">.claude/</code> directory
              with skills, MCP servers, and settings. Per-workflow config repos
              override conflicting files.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="global-config-repo">Repository</Label>
              <Input
                id="global-config-repo"
                placeholder="org/claude-config"
                value={globalConfigRepo}
                onChange={(e) => {
                  setGlobalConfigRepo(e.target.value)
                  setIsDirty(true)
                }}
              />
              <p className="text-xs text-muted-foreground">
                GitHub repo path. The repo must be accessible via the project's git
                provider connection. Leave empty to disable.
              </p>
            </div>
            <div className="space-y-2">
              <Label htmlFor="global-config-branch">Branch</Label>
              <Input
                id="global-config-branch"
                placeholder="main"
                value={globalConfigRepoBranch}
                onChange={(e) => {
                  setGlobalConfigRepoBranch(e.target.value)
                  setIsDirty(true)
                }}
              />
            </div>
          </CardContent>
        </Card>
      </div>

      {/* Sticky save bar — only mounts when dirty so the layout doesn't have a
          permanent footer competing with content. Slides in from the bottom
          and auto-dismisses when the user saves successfully. */}
      {isDirty && (
        <div className="fixed bottom-0 left-0 right-0 border-t bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/80 shadow-lg z-30">
          <div className="w-full px-4 sm:px-6 lg:px-8 py-3 flex items-center justify-between gap-3">
            <p className="text-sm text-muted-foreground">
              You have unsaved sandbox changes.
            </p>
            <Button onClick={handleSave} disabled={updateSettings.isPending} size="sm">
              {updateSettings.isPending ? (
                <Loader2 className="h-4 w-4 animate-spin mr-2" />
              ) : (
                <Save className="h-4 w-4 mr-2" />
              )}
              Save Changes
            </Button>
          </div>
        </div>
      )}
    </>
  )
}
