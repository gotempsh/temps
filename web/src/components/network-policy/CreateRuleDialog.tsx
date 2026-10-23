// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { problemDetail } from '@/lib/api-problem'
import {
  buildCreateRuleRequest,
  createNetworkPolicyRule,
  networkPolicyKeys,
  PEER_KINDS,
  peerRefHint,
  SUBJECT_KINDS,
  type CreateNetworkPolicyRuleRequest,
  type RuleFormValues,
} from '@/lib/network-policy'

const ruleFormSchema = z
  .object({
    subject_kind: z.enum([
      'deployment',
      'sandbox',
      'service',
      'function',
      'job',
    ]),
    subject_id: z.string(),
    direction: z.enum(['egress', 'ingress']),
    peer_kind: z.enum(['service', 'workload', 'cidr', 'host']),
    peer_ref: z.string(),
    port_start: z.string(),
    port_end: z.string(),
    protocol: z.enum(['tcp', 'udp', 'any']),
    action: z.enum(['allow', 'deny']),
    description: z.string(),
  })
  .superRefine((values, context) => {
    const result = buildCreateRuleRequest(values)
    if (!result.ok) {
      context.addIssue({
        code: 'custom',
        message: result.error,
        path: [result.field],
      })
    }
  })

const SUBJECT_LABELS: Record<RuleFormValues['subject_kind'], string> = {
  deployment: 'Deployment (environment ID)',
  sandbox: 'Sandbox',
  service: 'Service',
  function: 'Function',
  job: 'Job',
}

const PEER_LABELS: Record<RuleFormValues['peer_kind'], string> = {
  service: 'Service (by ID)',
  workload: 'Workload (kind/id)',
  cidr: 'CIDR block',
  host: 'Hostname (egress only)',
}

function defaults(
  subject?: Pick<RuleFormValues, 'subject_kind' | 'subject_id'>
): RuleFormValues {
  return {
    subject_kind: subject?.subject_kind ?? 'sandbox',
    subject_id: subject?.subject_id ?? '',
    direction: 'egress',
    peer_kind: 'host',
    peer_ref: '',
    port_start: '',
    port_end: '',
    protocol: 'tcp',
    action: 'allow',
    description: '',
  }
}

/** Create one operator rule. Controlled through `open`. */
export function CreateRuleDialog({
  open,
  onOpenChange,
  subject,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Prefill the subject, e.g. from the workload currently inspected. */
  subject?: Pick<RuleFormValues, 'subject_kind' | 'subject_id'>
}) {
  const queryClient = useQueryClient()
  const form = useForm<RuleFormValues>({
    resolver: zodResolver(ruleFormSchema),
    defaultValues: defaults(subject),
  })
  const create = useMutation({
    mutationFn: (request: CreateNetworkPolicyRuleRequest) =>
      createNetworkPolicyRule(request),
    meta: { errorTitle: 'Could not create the network policy rule' },
    onSuccess: async (rule) => {
      toast.success(`Rule #${rule.id} created`)
      await queryClient.invalidateQueries({ queryKey: networkPolicyKeys.all })
      form.reset(defaults(subject))
      onOpenChange(false)
    },
    onError: (error) => {
      toast.error('Could not create the network policy rule', {
        description: problemDetail(error, 'The server refused the rule.'),
      })
    },
  })
  const peerKind = useWatch({ control: form.control, name: 'peer_kind' })
  const subjectKind = useWatch({ control: form.control, name: 'subject_kind' })

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) form.reset(defaults(subject))
        onOpenChange(next)
      }}
    >
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Add network policy rule</DialogTitle>
          <DialogDescription>
            Allow or deny one workload&apos;s traffic to a peer — for example,
            let sandbox 42 reach registry.example.test over HTTPS. Rules apply
            on nodes whose network policy mode is Enforce.
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            noValidate
            className="space-y-4"
            onSubmit={form.handleSubmit((values) => {
              const result = buildCreateRuleRequest(values)
              if (result.ok) create.mutate(result.value)
            })}
          >
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <FormField
                control={form.control}
                name="subject_kind"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Workload kind</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {SUBJECT_KINDS.map((kind) => (
                          <SelectItem key={kind} value={kind}>
                            {SUBJECT_LABELS[kind]}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="subject_id"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>
                      {subjectKind === 'deployment'
                        ? 'Environment ID'
                        : 'Workload ID'}
                    </FormLabel>
                    <FormControl>
                      <Input inputMode="numeric" placeholder="42" {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="direction"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Direction</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value="egress">
                          Egress (outgoing)
                        </SelectItem>
                        <SelectItem value="ingress">
                          Ingress (incoming)
                        </SelectItem>
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="action"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Action</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value="allow">Allow</SelectItem>
                        <SelectItem value="deny">Deny</SelectItem>
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="peer_kind"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Peer kind</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {PEER_KINDS.map((kind) => (
                          <SelectItem key={kind} value={kind}>
                            {PEER_LABELS[kind]}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="peer_ref"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Peer</FormLabel>
                    <FormControl>
                      <Input
                        className="font-mono"
                        autoComplete="off"
                        spellCheck={false}
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>{peerRefHint(peerKind)}</FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="port_start"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>First port</FormLabel>
                    <FormControl>
                      <Input inputMode="numeric" placeholder="443" {...field} />
                    </FormControl>
                    <FormDescription>
                      Leave both ports empty for every port (80 and 443 for a
                      hostname).
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="port_end"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Last port</FormLabel>
                    <FormControl>
                      <Input
                        inputMode="numeric"
                        placeholder="Same as first"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="protocol"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Protocol</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value="tcp">TCP</SelectItem>
                        <SelectItem value="udp">UDP</SelectItem>
                        <SelectItem value="any">Any</SelectItem>
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="description"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Description (optional)</FormLabel>
                    <FormControl>
                      <Input placeholder="Pull images" {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                disabled={create.isPending}
                onClick={() => onOpenChange(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={create.isPending}>
                {create.isPending ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : null}
                Add rule
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
