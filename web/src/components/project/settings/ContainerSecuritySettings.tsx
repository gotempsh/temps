// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ContainerCapabilityOption } from '@/api/client'
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { SettingsSection } from '@/components/ui/settings-section'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  useContainerSecurity,
  withCapabilities,
} from '@/hooks/useContainerSecurity'
import { problemDetail } from '@/lib/api-problem'
import { ShieldCheck } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

export const CONTAINER_SECURITY_SECTION_ID = 'container-security'

const TITLE = 'Container security'
const DESCRIPTION =
  'Linux capabilities this service’s containers get back. Everything else stays dropped. Applies from the next deployment.'

export function ContainerSecuritySettings({
  projectId,
}: {
  projectId: number
}) {
  const { query, save, handleSensitiveActionError, verificationDialog } =
    useContainerSecurity(projectId)
  const [pending, setPending] = useState<ContainerCapabilityOption | null>(null)
  const [acknowledged, setAcknowledged] = useState(false)
  const data = query.data
  const allowed = data?.allowed_capabilities ?? []

  const change = (option: ContainerCapabilityOption, allow: boolean) => {
    if (!data) return
    const next = allow
      ? withCapabilities(allowed, [option.id])
      : allowed.filter((capability) => capability !== option.id)
    const run = () =>
      save.mutate(
        { allowed: next, expected: allowed, acknowledgeRisks: allow },
        {
          onSuccess: () => {
            setPending(null)
            setAcknowledged(false)
            toast.success(
              allow
                ? `${option.id} allowed. Redeploy to apply it.`
                : `${option.id} removed. Redeploy to apply full hardening.`
            )
          },
          onError: (error) => {
            if (handleSensitiveActionError(error, run)) return
            setPending(null)
            setAcknowledged(false)
            toast.error('Could not save container security', {
              description: problemDetail(error, 'Refresh and try again.'),
            })
          },
        }
      )
    run()
  }

  return (
    <SettingsSection
      id={CONTAINER_SECURITY_SECTION_ID}
      title={TITLE}
      description={DESCRIPTION}
      icon={ShieldCheck}
    >
      {query.isPending && (
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-14 w-full" />
          <Skeleton className="h-4 w-64" />
        </div>
      )}
      {query.isError && (
        <ErrorAlert
          title="Could not load container security"
          description={problemDetail(query.error, 'Try again.')}
          retry={() => void query.refetch()}
        />
      )}
      {data && (
        <div className="space-y-4">
          {!data.can_edit && (
            <p className="text-sm text-muted-foreground" role="note">
              {data.edit_requirement}
            </p>
          )}
          <div className="divide-y rounded-lg border">
            {data.options.map((option) => {
              const on = allowed.includes(option.id)
              return (
                <div key={option.id} className="flex items-start gap-3 p-3">
                  <div className="min-w-0 flex-1 space-y-1">
                    <label
                      htmlFor={`container-capability-${option.id}`}
                      className="text-sm font-medium [overflow-wrap:anywhere]"
                    >
                      {option.label}
                    </label>
                    <p className="text-xs text-muted-foreground">
                      {option.description}
                    </p>
                  </div>
                  <Switch
                    id={`container-capability-${option.id}`}
                    checked={on}
                    disabled={!data.can_edit || save.isPending}
                    onCheckedChange={(checked) => {
                      if (checked) {
                        setAcknowledged(false)
                        setPending(option)
                      } else {
                        change(option, false)
                      }
                    }}
                  />
                </div>
              )
            })}
          </div>
          <div className="space-y-1">
            <p className="text-xs font-medium text-muted-foreground">
              Always enforced
            </p>
            <ul className="list-disc space-y-0.5 pl-5 text-xs text-muted-foreground">
              {data.always_enforced.map((rule) => (
                <li key={rule}>{rule}</li>
              ))}
            </ul>
          </div>
        </div>
      )}
      <AlertDialog
        open={pending !== null}
        onOpenChange={(open) => {
          if (!open && !save.isPending) {
            setPending(null)
            setAcknowledged(false)
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Allow {pending?.id}?</AlertDialogTitle>
            <AlertDialogDescription>
              {pending?.description}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <div className="space-y-3 text-sm">
            <p>
              <span className="font-medium">Trade-off: </span>
              {pending?.trade_off}
            </p>
            <p className="text-muted-foreground">
              To keep full hardening instead, remove the file capability from
              the image (<code>setcap -r &lt;binary&gt;</code>) and listen on a
              port above 1024.
            </p>
            <div className="flex items-start gap-2">
              <Checkbox
                id="acknowledge-container-capability"
                checked={acknowledged}
                onCheckedChange={(value) => setAcknowledged(value === true)}
                disabled={save.isPending}
              />
              <label htmlFor="acknowledge-container-capability">
                I understand this weakens this service’s container hardening.
              </label>
            </div>
          </div>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={save.isPending}>
              Keep it dropped
            </AlertDialogCancel>
            <Button
              disabled={!acknowledged || save.isPending}
              onClick={() => {
                if (pending) change(pending, true)
              }}
            >
              {save.isPending ? 'Saving…' : `Allow ${pending?.id ?? ''}`}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      {verificationDialog}
    </SettingsSection>
  )
}
