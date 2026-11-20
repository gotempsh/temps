// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ShieldCheck } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import {
  describePorts,
  describeSubject,
  effectivePolicyEnforcementText,
  getEffectivePolicy,
  networkPolicyKeys,
  profileUpdate,
  setSubjectProfile,
  type EffectivePolicy,
  type Profile,
  type SetProfileRequest,
  type SubjectKind,
} from '@/lib/network-policy'
import { isForbiddenRead } from '@/lib/read-failure'

const DEFAULT_VALUE = 'default'
const PROFILE_OPTIONS: { value: Profile; label: string }[] = [
  { value: 'open', label: 'Open — reaches anything' },
  { value: 'restricted', label: 'Restricted — only rules and links' },
  { value: 'none', label: 'None — reaches nothing' },
]

function ProfileSelect({
  id,
  label,
  current,
  source,
  disabled,
  onChange,
}: {
  id: string
  label: string
  current: string
  source: string
  disabled: boolean
  onChange: (value: Profile | undefined) => void
}) {
  return (
    <div className="space-y-1.5">
      <label htmlFor={id} className="text-sm font-medium">
        {label}
      </label>
      <Select
        value={source === 'override' ? current : DEFAULT_VALUE}
        disabled={disabled}
        onValueChange={(value) =>
          onChange(value === DEFAULT_VALUE ? undefined : (value as Profile))
        }
      >
        <SelectTrigger id={id}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={DEFAULT_VALUE}>
            Kind default ({source === 'default' ? current : 'see kind'})
          </SelectItem>
          {PROFILE_OPTIONS.map((option) => (
            <SelectItem key={option.value} value={option.value}>
              {option.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <p className="text-xs text-muted-foreground">
        Effective: <span className="font-medium">{current}</span> (
        {source === 'override' ? 'override' : "the kind's default"})
      </p>
    </div>
  )
}

function InboundLinks({ policy }: { policy: EffectivePolicy }) {
  if (policy.inbound_links.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        No other workload is linked to this one.
      </p>
    )
  }
  return (
    <ul className="divide-y rounded-lg border text-sm">
      {policy.inbound_links.map((rule) => (
        <li key={rule.id} className="flex flex-wrap items-center gap-2 p-3">
          <Badge variant="neutral-subtle">link</Badge>
          <span>
            {describeSubject(rule.subject_kind, rule.subject_id)} may reach this
            workload on {describePorts(rule)}
          </span>
        </li>
      ))}
    </ul>
  )
}

/**
 * The effective network policy of one workload (ADR-055 §9): its egress and
 * ingress profiles, where they are enforced, and who is linked to it.
 */
export function SubjectPolicyPanel({
  subjectKind,
  subjectId,
  canEdit,
}: {
  subjectKind: SubjectKind
  subjectId: number
  canEdit: boolean
}) {
  const queryClient = useQueryClient()
  const queryKey = networkPolicyKeys.subject(subjectKind, subjectId)
  const query = useQuery({
    queryKey,
    queryFn: () => getEffectivePolicy(subjectKind, subjectId),
    meta: { errorTitle: 'Could not load the effective policy' },
  })
  const update = useMutation({
    mutationFn: (request: SetProfileRequest) =>
      setSubjectProfile(subjectKind, subjectId, request),
    meta: { errorTitle: 'Could not change the profile' },
    onSuccess: (profile) => {
      queryClient.setQueryData<EffectivePolicy>(queryKey, (previous) =>
        previous ? { ...previous, profile } : previous
      )
      toast.success(
        `Profiles for ${describeSubject(subjectKind, subjectId)} updated`
      )
    },
    onError: (error) => {
      toast.error('Could not change the profile', {
        description: problemDetail(error, 'The server refused the change.'),
      })
    },
  })
  const title = `Effective policy of ${describeSubject(subjectKind, subjectId)}`

  return (
    <section
      aria-label={title}
      className="space-y-4 rounded-lg border bg-card p-4 text-card-foreground"
    >
      <div className="flex items-center gap-2">
        <ShieldCheck className="h-4 w-4 text-muted-foreground" />
        <h2 className="text-base font-semibold">{title}</h2>
      </div>
      {query.isPending ? (
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-4 w-1/2" />
        </div>
      ) : query.isError ? (
        <ReadFailure
          resource="Effective policy"
          error={query.error}
          embedded
          onRetry={
            isForbiddenRead(query.error)
              ? undefined
              : () => void query.refetch()
          }
          retrying={query.isFetching}
        />
      ) : (
        <div className="space-y-4">
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <Badge
              variant={
                query.data.enforced ? 'success-subtle' : 'neutral-subtle'
              }
            >
              {query.data.enforced ? 'Enforced' : 'Not enforced'}
            </Badge>
            <span className="text-muted-foreground">
              {effectivePolicyEnforcementText(query.data)}
            </span>
          </div>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <ProfileSelect
              id={`egress-${subjectKind}-${subjectId}`}
              label="Egress profile"
              current={query.data.profile.egress}
              source={query.data.profile.egress_source}
              disabled={!canEdit || update.isPending}
              onChange={(egress) =>
                update.mutate(
                  profileUpdate(query.data.profile, 'egress', egress)
                )
              }
            />
            <ProfileSelect
              id={`ingress-${subjectKind}-${subjectId}`}
              label="Ingress profile"
              current={query.data.profile.ingress}
              source={query.data.profile.ingress_source}
              disabled={!canEdit || update.isPending}
              onChange={(ingress) =>
                update.mutate(
                  profileUpdate(query.data.profile, 'ingress', ingress)
                )
              }
            />
          </div>
          {canEdit ? null : (
            <p className="text-sm text-muted-foreground">
              Read-only: only instance admins can change profiles.
            </p>
          )}
          <div className="space-y-2">
            <h3 className="text-sm font-medium">
              Inbound links ({query.data.inbound_links.length})
            </h3>
            <p className="text-sm text-muted-foreground">
              Link rules of other workloads that name this one. They are
              accepted even when this workload&apos;s ingress is restricted.
            </p>
            <InboundLinks policy={query.data} />
          </div>
          <p className="text-sm text-muted-foreground">
            This workload&apos;s own rules ({query.data.rules.length}) are
            listed in the table below.
          </p>
        </div>
      )}
    </section>
  )
}
