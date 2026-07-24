import { Button } from '@/components/ui/button'
import { useQuery } from '@tanstack/react-query'
import { Check, ExternalLink, Loader2, Wand2 } from 'lucide-react'
import { useNavigate } from 'react-router-dom'
import {
  getRunOptions,
  latestRunForSourceOptions,
} from '@/api/client/@tanstack/react-query.gen'

interface AutofixButtonProps {
  projectId: number
  projectSlug: string
  errorGroupId: number
}

export function AutofixButton({ projectId, projectSlug, errorGroupId }: AutofixButtonProps) {
  const navigate = useNavigate()

  const { data: latestAgentRun } = useQuery({
    ...latestRunForSourceOptions({
      path: { project_id: projectId },
      query: {
        trigger_source_type: 'error_group',
        trigger_source_id: errorGroupId,
      },
    }),
    refetchInterval: 10000,
  })

  const runId = latestAgentRun?.id
  const { data: runWithLogs } = useQuery({
    ...getRunOptions({
      path: { project_id: projectId, run_id: runId ?? 0 },
    }),
    enabled: runId !== undefined,
    refetchInterval: 10000,
  })

  const latestRun = runWithLogs?.run
  const errorMessage = latestRun?.error_message || 'Something went wrong'
  const errorSummary =
    errorMessage.length > 160 ? errorMessage.slice(0, 160) + '…' : errorMessage
  const goToAutofix = () => navigate(`/projects/${projectSlug}/errors/${errorGroupId}/autofix`)

  const phase = latestRun?.phase || latestRun?.status

  return (
    <div className="rounded-lg border bg-card p-4">
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3">
        <div className="flex items-center gap-3">
          <div className={`flex items-center justify-center w-8 h-8 rounded-lg flex-shrink-0 ${
            !latestRun ? 'bg-muted' :
            phase === 'completed' ? 'bg-green-500/10' :
            ['analyzing', 'fixing'].includes(phase || '') ? 'bg-blue-500/10' :
            ['failed', 'cancelled'].includes(latestRun?.status || '') ? 'bg-red-500/10' :
            'bg-blue-500/10'
          }`}>
            {!latestRun && <Wand2 className="h-4 w-4 text-muted-foreground" />}
            {latestRun && ['analyzing', 'fixing'].includes(phase || '') && (
              <Loader2 className="h-4 w-4 text-blue-400 animate-spin" />
            )}
            {latestRun && phase === 'completed' && <Check className="h-4 w-4 text-green-400" />}
            {latestRun && ['analyzed', 'fix_ready'].includes(phase || '') && (
              <Wand2 className="h-4 w-4 text-blue-400" />
            )}
            {latestRun && ['failed', 'cancelled'].includes(latestRun.status) && (
              <Wand2 className="h-4 w-4 text-red-400" />
            )}
          </div>
          <div>
            <p className="text-sm font-medium">
              {!latestRun && 'Autofix'}
              {latestRun && phase === 'analyzing' && 'Analyzing error...'}
              {latestRun && phase === 'analyzed' && 'Analysis ready — review before fixing'}
              {latestRun && phase === 'fixing' && 'Generating fix...'}
              {latestRun && phase === 'fix_ready' && 'Fix ready — review before creating PR'}
              {latestRun && phase === 'completed' && 'Fix applied'}
              {latestRun && latestRun.status === 'failed' && 'Autofix failed'}
              {latestRun && latestRun.status === 'cancelled' && 'Autofix cancelled'}
            </p>
            <p className="text-xs text-muted-foreground line-clamp-2 break-words">
              {!latestRun && 'Let AI analyze and fix this error automatically'}
              {latestRun && phase === 'analyzing' && 'Claude is reading your codebase and tracing the error'}
              {latestRun && phase === 'analyzed' && 'Root cause identified — click to review the analysis'}
              {latestRun && phase === 'fixing' && 'Claude is writing the fix and tests'}
              {latestRun && phase === 'fix_ready' && 'Code changes are ready for review'}
              {latestRun && phase === 'completed' && latestRun.pr_url && `PR #${latestRun.pr_number} created`}
              {latestRun && phase === 'completed' && !latestRun.pr_url && 'Completed'}
              {latestRun && latestRun.status === 'failed' && (
                <span title={errorMessage.length > 160 ? errorMessage : undefined}>
                  {errorSummary}
                </span>
              )}
              {latestRun && latestRun.status === 'cancelled' && 'Cancelled by user'}
            </p>
          </div>
        </div>
        <div className="flex gap-2 flex-shrink-0">
          {latestRun?.pr_url && (
            <Button variant="outline" size="sm" asChild>
              <a href={latestRun.pr_url} target="_blank" rel="noopener noreferrer">
                PR #{latestRun.pr_number} <ExternalLink className="h-3 w-3 ml-1" />
              </a>
            </Button>
          )}
          {!latestRun && (
            <Button size="sm" onClick={goToAutofix}>
              <Wand2 className="h-4 w-4 mr-1" />
              Fix with AI
            </Button>
          )}
          {latestRun && ['analyzed', 'fix_ready'].includes(phase || '') && (
            <Button size="sm" onClick={goToAutofix}>
              Review
            </Button>
          )}
          {latestRun && ['analyzing', 'fixing'].includes(phase || '') && (
            <Button variant="outline" size="sm" onClick={goToAutofix}>
              View progress
            </Button>
          )}
          {latestRun && ['failed', 'cancelled', 'completed'].includes(latestRun.status) && (
            <Button variant="outline" size="sm" onClick={goToAutofix}>
              {latestRun.status === 'completed' ? 'View' : 'Retry'}
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}
