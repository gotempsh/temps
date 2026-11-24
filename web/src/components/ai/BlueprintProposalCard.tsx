// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  acceptBlueprintVersionMutation,
  getBlueprintOptions,
  getChatWorkspaceQueryKey,
  rejectBlueprintVersionMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ChatWorkspaceResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, Loader2, Network, Pencil, X } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { BlueprintDiagram } from './BlueprintDiagram'
import { blueprintChanges } from './blueprint-layout'
import {
  acceptedMessage,
  continueChat,
  invalidateBlueprint,
  rejectedMessage,
  proposalState,
  storeBlueprint,
  type BlueprintProposalReceipt,
  type ProposalState,
} from './chat-blueprint'

const STATE_LABELS: Record<Exclude<ProposalState, 'open'>, string> = {
  accepted: 'Accepted',
  rejected: 'Rejected',
  superseded: 'Replaced by a newer version',
  unknown: '',
}

/**
 * The architecture the assistant proposed, in the transcript where it was
 * proposed, with the decision the assistant is waiting for. Accepting or
 * rejecting tells the chat, so the user never has to type "go ahead".
 */
export function BlueprintProposalCard({
  conversationPublicId,
  receipt,
}: {
  conversationPublicId: string
  receipt: BlueprintProposalReceipt
}) {
  const queryClient = useQueryClient()
  const blueprint = useQuery(
    getBlueprintOptions({ path: { public_id: receipt.applicationId } })
  )
  const state = proposalState(blueprint.data, receipt.version)
  // Proposing can create the chat's application; make sure the rest of the
  // chat (its tabs, its workspace chip) knows the chat is in it.
  useEffect(() => {
    const key = getChatWorkspaceQueryKey({
      path: { public_id: conversationPublicId },
    })
    const known = queryClient.getQueryData<ChatWorkspaceResponse>(key)
    if (known?.workspace?.public_id !== receipt.applicationId) {
      void queryClient.invalidateQueries({ queryKey: key })
    }
  }, [queryClient, conversationPublicId, receipt.applicationId])
  const [rejecting, setRejecting] = useState(false)
  const [reason, setReason] = useState('')
  const path = {
    public_id: receipt.applicationId,
    version: receipt.version,
  }
  const accept = useMutation({
    ...acceptBlueprintVersionMutation(),
    meta: { errorTitle: 'Could not accept the architecture' },
    onSuccess: (data) => {
      storeBlueprint(queryClient, data)
      invalidateBlueprint(
        queryClient,
        receipt.applicationId,
        conversationPublicId
      )
      toast.success(`Architecture version ${receipt.version} accepted`)
      continueChat(conversationPublicId, acceptedMessage(receipt.version))
    },
  })
  const reject = useMutation({
    ...rejectBlueprintVersionMutation(),
    meta: { errorTitle: 'Could not reject the architecture' },
    onSuccess: (data) => {
      storeBlueprint(queryClient, data)
      invalidateBlueprint(
        queryClient,
        receipt.applicationId,
        conversationPublicId
      )
      setRejecting(false)
    },
  })
  // "Change something" asks the chat for a new design; a plain rejection only
  // takes the proposal off the table and keeps whatever was accepted before.
  const rejectAndAsk = () =>
    reject.mutate(
      { path },
      {
        onSuccess: () =>
          continueChat(
            conversationPublicId,
            rejectedMessage(receipt.version, reason)
          ),
      }
    )
  const rejectOnly = () =>
    reject.mutate(
      { path },
      {
        onSuccess: (data) =>
          toast.success(
            data.accepted
              ? `Version ${receipt.version} rejected. Version ${data.accepted.version} stays as it is.`
              : `Version ${receipt.version} rejected`
          ),
      }
    )
  // Compare with what was accepted when this was proposed, when that is
  // still the accepted version.
  const previous =
    blueprint.data?.accepted &&
    blueprint.data.accepted.version < receipt.version
      ? blueprint.data.accepted.document
      : null
  const changes = blueprintChanges(
    previous
      ? { components: previous.components, flows: previous.flows ?? [] }
      : null,
    {
      components: receipt.document.components,
      flows: receipt.document.flows ?? [],
    }
  )
  const busy = accept.isPending || reject.isPending

  return (
    <section
      aria-label={`Proposed architecture, version ${receipt.version}`}
      className="my-2 space-y-3 rounded-lg border bg-card p-3"
    >
      <header className="flex flex-wrap items-start gap-2">
        <Network
          className="mt-0.5 size-4 shrink-0 text-muted-foreground"
          aria-hidden
        />
        <div className="min-w-0 flex-1 basis-56">
          <p className="text-sm font-medium">
            Proposed architecture
            {receipt.workspaceName && (
              <span className="font-normal text-muted-foreground">
                {' '}
                · {receipt.workspaceName} · version {receipt.version}
              </span>
            )}
          </p>
          <p
            className="line-clamp-3 text-sm text-muted-foreground"
            title={receipt.document.summary}
          >
            {receipt.document.summary}
          </p>
          {receipt.changeSummary && (
            <p className="mt-1 text-xs text-muted-foreground">
              {receipt.changeSummary}
            </p>
          )}
        </div>
        {blueprint.isPending ? (
          <Skeleton className="h-5 w-20" />
        ) : (
          state !== 'open' &&
          state !== 'unknown' && (
            <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[11px] font-medium text-muted-foreground">
              {STATE_LABELS[state]}
            </span>
          )
        )}
      </header>
      <BlueprintDiagram
        document={receipt.document}
        changes={previous ? changes : undefined}
      />
      {changes.removed.length > 0 && (
        <p className="text-xs text-muted-foreground">
          Removes {changes.removed.map((c) => c.name).join(', ')}.
        </p>
      )}
      {state === 'open' && !rejecting && (
        <div className="flex flex-wrap items-center gap-2">
          <Button
            type="button"
            size="sm"
            className="gap-1.5"
            disabled={busy}
            onClick={() => accept.mutate({ path })}
          >
            {accept.isPending ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <Check className="size-4" />
            )}
            Accept and build
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            className="gap-1.5"
            disabled={busy}
            onClick={() => setRejecting(true)}
          >
            <Pencil className="size-4" />
            Change something
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="gap-1.5"
            disabled={busy}
            onClick={rejectOnly}
            title="Discard this proposal without asking for another one"
          >
            {reject.isPending ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <X className="size-4" />
            )}
            Reject
          </Button>
          <span className="text-xs text-muted-foreground">
            Nothing is built until you accept.
          </span>
        </div>
      )}
      {state === 'open' && rejecting && (
        <div className="space-y-2">
          <Textarea
            value={reason}
            onChange={(event) => setReason(event.target.value)}
            placeholder="What should change? For example: use Redis Streams instead of a queue service, and drop the worker."
            rows={2}
            aria-label="What should change"
          />
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={busy}
              onClick={rejectAndAsk}
            >
              {reject.isPending && <Loader2 className="size-4 animate-spin" />}
              Reject and send
            </Button>
            <Button
              type="button"
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() => setRejecting(false)}
            >
              Cancel
            </Button>
          </div>
        </div>
      )}
    </section>
  )
}
