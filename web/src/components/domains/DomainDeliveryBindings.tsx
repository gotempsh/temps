import {
  deleteDomainDeliveryBinding,
  listDomainDeliveryBindings,
  type DomainDeliveryBindingResponse,
} from '@/api/client'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Link } from 'react-router-dom'
import { deliveryError, requireDeliveryData } from './delivery-errors'

export function DomainDeliveryBindings({
  projectId,
  onConfigure,
}: {
  projectId: number
  onConfigure: (
    hostname: string,
    environmentId: number,
    binding?: DomainDeliveryBindingResponse
  ) => void
}) {
  const client = useQueryClient()
  const [removing, setRemoving] =
    useState<DomainDeliveryBindingResponse | null>(null)
  const remove = useMutation({
    mutationFn: async (bindingId: number) => {
      const response = await deleteDomainDeliveryBinding({
        path: { project_id: projectId, binding_id: bindingId },
      })
      if (response.error) throw new Error(deliveryError(response.error))
    },
    onSuccess: () => {
      setRemoving(null)
      toast.success('Managed DNS removed. The domain route is preserved.')
    },
    onSettled: () =>
      client.invalidateQueries({ queryKey: ['delivery-bindings', projectId] }),
  })
  const bindings = useQuery({
    queryKey: ['delivery-bindings', projectId],
    queryFn: async () =>
      requireDeliveryData(
        await listDomainDeliveryBindings({ path: { project_id: projectId } })
      ),
  })
  return (
    <section aria-labelledby="delivery-bindings-title" className="space-y-3">
      <h3 id="delivery-bindings-title" className="font-semibold">
        Managed delivery
      </h3>
      {bindings.isPending ? (
        <Skeleton className="h-24 w-full" />
      ) : bindings.isError ? (
        <Alert variant="destructive">
          <AlertDescription>
            {deliveryError(bindings.error)}{' '}
            <Button variant="link" onClick={() => bindings.refetch()}>
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      ) : !bindings.data?.length ? (
        <p className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
          No managed delivery bindings yet. Configure a hostname to inspect its
          DNS records and choose how traffic reaches this project.
        </p>
      ) : (
        <ul className="divide-y rounded-lg border">
          {bindings.data.map((binding) => (
            <li key={binding.id} className="space-y-3 p-4">
              <div className="flex flex-col justify-between gap-3 sm:flex-row sm:items-start">
                <div className="min-w-0">
                  <p className="break-all font-medium">{binding.hostname}</p>
                  <p className="mt-1 text-sm text-muted-foreground">
                    {binding.delivery_profile_name} · {binding.origin_target}
                  </p>
                  <p className="mt-1 text-xs text-muted-foreground">
                    Applied from {binding.profile_source.replace(/_/g, ' ')}.
                    Defaults do not change this binding automatically.
                  </p>
                  {binding.status === 'dns_configured' && (
                    <p className="mt-2 text-xs text-muted-foreground">
                      DNS is configured. Verify public HTTPS before sending
                      traffic.{' '}
                      <Link className="underline" to="/certificates">
                        Manage origin certificates
                      </Link>
                    </p>
                  )}
                </div>
                <div className="flex shrink-0 flex-wrap items-center gap-3">
                  <Badge
                    variant={binding.last_error ? 'destructive' : 'secondary'}
                  >
                    {binding.status.replace(/_/g, ' ')}
                  </Badge>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() =>
                      onConfigure(
                        binding.hostname,
                        binding.environment_id,
                        binding
                      )
                    }
                  >
                    {binding.last_error ? 'Review and retry' : 'Change setup'}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={binding.status === 'applying'}
                    onClick={() => {
                      remove.reset()
                      setRemoving(binding)
                    }}
                  >
                    Remove managed DNS
                  </Button>
                  {binding.status === 'dns_configured' && (
                    <Button size="sm" variant="ghost" asChild>
                      <a
                        href={`https://${binding.hostname}`}
                        target="_blank"
                        rel="noreferrer"
                      >
                        Open HTTPS
                      </a>
                    </Button>
                  )}
                </div>
              </div>
              {binding.last_error && (
                <Alert variant="destructive">
                  <AlertDescription>{binding.last_error}</AlertDescription>
                </Alert>
              )}
            </li>
          ))}
        </ul>
      )}
      <Dialog
        open={!!removing}
        onOpenChange={(open) => {
          if (!open && !remove.isPending) setRemoving(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Remove managed DNS?</DialogTitle>
            <DialogDescription>
              This removes the owned DNS record for {removing?.hostname} and can
              interrupt traffic. The domain route and certificate stay in Temps.
              You can delete the domain route afterward.
            </DialogDescription>
          </DialogHeader>
          {remove.isError && (
            <Alert variant="destructive">
              <AlertDescription>{deliveryError(remove.error)}</AlertDescription>
            </Alert>
          )}
          <DialogFooter>
            <Button
              variant="outline"
              disabled={remove.isPending}
              onClick={() => setRemoving(null)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={remove.isPending}
              onClick={() => removing && remove.mutate(removing.id)}
            >
              {remove.isPending ? 'Removing…' : 'Remove managed DNS'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  )
}
