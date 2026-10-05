// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, CheckCircle2, KeyRound, Loader2 } from 'lucide-react'
import {
  nodeSshEnrollmentCreateMutation,
  nodeSshEnrollmentGetOptions,
  nodeSshEnrollmentListOptions,
  nodeSshHostKeyMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  NodeSshCredentials,
  NodeSshEnrollmentSummary,
  NodeSshHostKeyResponse,
  WireguardMeshStatusResponse,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { QueryErrorAlert, TONE_CLASSES } from '@/components/nodes/mesh-ui'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import { isStepUpRequired } from '@/lib/sensitiveActionProblem'
import {
  hostKeyCompareCommand,
  hostKeyFileForAlgorithm,
} from '@/lib/ssh-host-key'
import { enrollmentProgress, SSH_ENROLLMENT_STEPS } from '@/lib/wireguard-mesh'

type AuthMethod = NodeSshCredentials['method']

/**
 * Add a server over SSH (ADR 048 D2c): read and confirm its host key, then
 * the control plane logs in, installs Temps if needed, pairs it and starts
 * its agent. Credentials are sent once and never stored.
 */
export function SshEnrollNode({ mesh }: { mesh: WireguardMeshStatusResponse }) {
  const queryClient = useQueryClient()
  const [host, setHost] = useState('')
  const [port, setPort] = useState('22')
  const [user, setUser] = useState('root')
  const [method, setMethod] = useState<AuthMethod>('password')
  const [password, setPassword] = useState('')
  const [privateKey, setPrivateKey] = useState('')
  const [passphrase, setPassphrase] = useState('')
  const [name, setName] = useState('')
  const [nodeAddress, setNodeAddress] = useState('')
  const [hostKey, setHostKey] = useState<NodeSshHostKeyResponse | null>(null)
  const [enrollmentId, setEnrollmentId] = useState<number | null>(null)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const portNumber = Number.parseInt(port, 10) || 22
  const target = { host: host.trim(), port: portNumber }

  const probe = useMutation({
    ...nodeSshHostKeyMutation(),
    onSuccess: (data) => setHostKey(data),
    onError: (error, variables) => {
      handleSensitiveActionError(error, () => probe.mutate(variables))
    },
  })

  const clearSecrets = () => {
    setPassword('')
    setPrivateKey('')
    setPassphrase('')
  }

  const create = useMutation({
    ...nodeSshEnrollmentCreateMutation(),
    onSuccess: async (data) => {
      clearSecrets()
      setHostKey(null)
      setEnrollmentId(data.id)
      await queryClient.invalidateQueries({
        queryKey: nodeSshEnrollmentListOptions().queryKey,
      })
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => create.mutate(variables)))
        return
    },
  })

  const credentials = (): NodeSshCredentials =>
    method === 'password'
      ? { method: 'password', password }
      : method === 'private_key'
        ? {
            method: 'private_key',
            private_key: privateKey,
            passphrase: passphrase || null,
          }
        : { method: 'agent' }

  const credentialsReady =
    method === 'agent' ||
    (method === 'password' ? password !== '' : privateKey.trim() !== '')

  const resetHostKey = () => {
    setHostKey(null)
    probe.reset()
  }

  return (
    <div className="space-y-3">
      <p>
        This server logs in to the worker over SSH, installs Temps if needed,
        pairs it and starts its agent. The worker needs Linux and Docker, and
        UDP <code>{mesh.listen_port}</code> open to this server. The credentials
        are used for this once and never stored.
      </p>

      <form
        className="space-y-3"
        onSubmit={(event) => {
          event.preventDefault()
          resetHostKey()
          probe.mutate({ body: target })
        }}
      >
        <div className="grid gap-2 sm:grid-cols-[1fr_6rem_10rem]">
          <div className="space-y-1">
            <Label htmlFor="ssh-host">Server</Label>
            <Input
              id="ssh-host"
              placeholder="203.0.113.10 or node.example.com"
              value={host}
              onChange={(event) => {
                setHost(event.target.value)
                resetHostKey()
              }}
              required
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="ssh-port">Port</Label>
            <Input
              id="ssh-port"
              inputMode="numeric"
              value={port}
              onChange={(event) => {
                setPort(event.target.value)
                resetHostKey()
              }}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="ssh-user">User</Label>
            <Input
              id="ssh-user"
              value={user}
              onChange={(event) => setUser(event.target.value)}
              required
            />
          </div>
        </div>

        <div className="space-y-2">
          <Label>Log in with</Label>
          <RadioGroup
            className="flex flex-wrap gap-4"
            value={method}
            onValueChange={(value) => setMethod(value as AuthMethod)}
          >
            {(
              [
                ['password', 'Password'],
                ['private_key', 'Private key'],
                ['agent', "This server's SSH agent"],
              ] as const
            ).map(([value, label]) => (
              <div key={value} className="flex items-center gap-1.5">
                <RadioGroupItem value={value} id={`ssh-auth-${value}`} />
                <Label htmlFor={`ssh-auth-${value}`} className="font-normal">
                  {label}
                </Label>
              </div>
            ))}
          </RadioGroup>
          {method === 'password' && (
            <Input
              type="password"
              autoComplete="off"
              placeholder={`Password for ${user || 'the user'}`}
              value={password}
              onChange={(event) => setPassword(event.target.value)}
            />
          )}
          {method === 'private_key' && (
            <>
              <Textarea
                className="font-mono text-xs"
                rows={4}
                spellCheck={false}
                placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
                value={privateKey}
                onChange={(event) => setPrivateKey(event.target.value)}
              />
              <Input
                type="password"
                autoComplete="off"
                placeholder="Passphrase (if the key has one)"
                value={passphrase}
                onChange={(event) => setPassphrase(event.target.value)}
              />
            </>
          )}
          {method === 'agent' && (
            <p className="text-xs">
              Uses the keys loaded in the SSH agent of this server&apos;s{' '}
              <code>temps serve</code> process (<code>SSH_AUTH_SOCK</code>).
            </p>
          )}
          {method !== 'agent' && user !== 'root' && (
            <p className="text-xs">
              {user || 'The user'} needs sudo.
              {method === 'password'
                ? ' If sudo asks for a password, this one is used.'
                : ' It must not ask for a password; log in with a password otherwise.'}
            </p>
          )}
        </div>

        <details className="text-xs">
          <summary className="cursor-pointer">
            Name and address (optional)
          </summary>
          <div className="mt-2 grid gap-2 sm:grid-cols-2">
            <div className="space-y-1">
              <Label htmlFor="ssh-name">Node name</Label>
              <Input
                id="ssh-name"
                placeholder="worker-1"
                value={name}
                onChange={(event) => setName(event.target.value)}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="ssh-node-address">
                Public IP for WireGuard, if not the server address
              </Label>
              <Input
                id="ssh-node-address"
                placeholder="203.0.113.10"
                value={nodeAddress}
                onChange={(event) => setNodeAddress(event.target.value)}
              />
            </div>
          </div>
        </details>

        {!hostKey && (
          <Button
            type="submit"
            disabled={!target.host || !user.trim() || probe.isPending}
          >
            {probe.isPending ? (
              <Loader2 className="mr-1 h-4 w-4 animate-spin" />
            ) : (
              <KeyRound className="mr-1 h-4 w-4" />
            )}
            Check the host key
          </Button>
        )}
      </form>

      {probe.isError && !isStepUpRequired(probe.error) && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Could not read the host key</AlertTitle>
          <AlertDescription>
            {problemDetail(probe.error, 'Check the address and port.')}
          </AlertDescription>
        </Alert>
      )}

      {hostKey && (
        <div className="space-y-2 rounded-md border bg-background p-3">
          <p className="text-foreground">
            {hostKey.address} presents this {hostKey.algorithm} host key:
          </p>
          <p className="break-all font-mono text-xs text-foreground">
            {hostKey.fingerprint}
          </p>
          <p className="text-xs">
            Compare it with the server&apos;s own, from its console or a session
            you trust:{' '}
            <code className="break-all">
              {hostKeyCompareCommand(hostKey.algorithm)}
            </code>
            .{' '}
            {hostKeyFileForAlgorithm(hostKey.algorithm)
              ? 'If they differ, do not continue: something else answered.'
              : 'If none of them matches, do not continue: something else answered.'}
          </p>
          {create.isError && !isStepUpRequired(create.error) && (
            <Alert variant="destructive">
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>Could not start</AlertTitle>
              <AlertDescription>
                {problemDetail(create.error, 'Try again.')}
              </AlertDescription>
            </Alert>
          )}
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              disabled={!credentialsReady || create.isPending}
              onClick={() =>
                create.mutate({
                  body: {
                    ...target,
                    user: user.trim(),
                    credentials: credentials(),
                    host_key_fingerprint: hostKey.fingerprint,
                    name: name.trim() || null,
                    node_address: nodeAddress.trim() || null,
                  },
                })
              }
            >
              {create.isPending && (
                <Loader2 className="mr-1 h-4 w-4 animate-spin" />
              )}
              It matches: add the server
            </Button>
            <Button type="button" variant="outline" onClick={resetHostKey}>
              Cancel
            </Button>
          </div>
          {!credentialsReady && (
            <p className="text-xs">
              Enter the {method === 'password' ? 'password' : 'private key'}{' '}
              above first.
            </p>
          )}
        </div>
      )}

      {enrollmentId !== null && <EnrollmentProgress id={enrollmentId} />}
      <RecentEnrollments
        current={enrollmentId}
        onSelect={(id) => setEnrollmentId(id)}
      />
      {verificationDialog}
    </div>
  )
}

/** One enrollment's steps and log, polled while it runs. */
function EnrollmentProgress({ id }: { id: number }) {
  const { data, error, refetch, isFetching } = useQuery({
    ...nodeSshEnrollmentGetOptions({ path: { enrollment_id: id } }),
    refetchInterval: (query) =>
      query.state.data?.status === 'running' ? 2_000 : false,
  })
  if (error) {
    return (
      <QueryErrorAlert
        title="Could not read the progress"
        error={error}
        onRetry={() => void refetch()}
        retrying={isFetching}
      />
    )
  }
  if (!data) {
    return (
      <div className="space-y-2 rounded-md border bg-background p-3">
        <Skeleton className="h-4 w-1/2" />
        {SSH_ENROLLMENT_STEPS.map((step) => (
          <Skeleton key={step} className="h-3 w-40" />
        ))}
      </div>
    )
  }
  const reached = SSH_ENROLLMENT_STEPS.indexOf(data.step)

  return (
    <div className="space-y-2 rounded-md border bg-background p-3 text-xs">
      <p className="font-medium text-foreground">
        {data.name}{' '}
        <span className="font-mono text-muted-foreground">
          {data.ssh_user}@{data.ssh_address}
        </span>
      </p>
      <ol className="space-y-0.5">
        {SSH_ENROLLMENT_STEPS.map((step, index) => {
          const done =
            data.status === 'succeeded' || (reached >= 0 && index < reached)
          const current = index === reached && data.status !== 'succeeded'
          return (
            <li key={step} className="flex items-center gap-1.5">
              {done ? (
                <CheckCircle2 className="h-3.5 w-3.5 text-green-600" />
              ) : current && data.status === 'running' ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : current ? (
                <AlertTriangle className="h-3.5 w-3.5 text-red-600" />
              ) : (
                <span className="inline-block h-3.5 w-3.5" />
              )}
              <span className={current ? 'text-foreground' : undefined}>
                {step}
              </span>
            </li>
          )
        })}
      </ol>
      {data.status === 'succeeded' && (
        <Alert>
          <CheckCircle2 className="h-4 w-4 text-green-600" />
          <AlertTitle>{data.name} was added</AlertTitle>
          <AlertDescription>
            {data.agent_mode === 'detached' ? (
              <>
                The server has no systemd, so its agent was started in the
                background and will not come back after a reboot. Run{' '}
                <code>temps agent</code> there under a supervisor.
              </>
            ) : (
              <>
                Its agent runs as <code>temps-agent.service</code> and restarts
                on failure and at boot.
              </>
            )}
          </AlertDescription>
        </Alert>
      )}
      {data.status === 'failed' && data.error && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Adding {data.name} failed</AlertTitle>
          <AlertDescription className="whitespace-pre-wrap">
            {data.error}
          </AlertDescription>
        </Alert>
      )}
      {data.log && (
        <pre className="max-h-64 overflow-auto rounded bg-muted p-2 font-mono text-[11px] leading-snug">
          {data.log}
        </pre>
      )}
    </div>
  )
}

function RecentEnrollments({
  current,
  onSelect,
}: {
  current: number | null
  onSelect: (id: number) => void
}) {
  const { data, error, refetch, isFetching } = useQuery({
    ...nodeSshEnrollmentListOptions(),
    refetchInterval: (query) =>
      query.state.data?.enrollments.some((e) => e.status === 'running')
        ? 3_000
        : 30_000,
  })
  if (error) {
    return (
      <QueryErrorAlert
        title="Could not read the servers added over SSH"
        error={error}
        onRetry={() => void refetch()}
        retrying={isFetching}
      />
    )
  }
  const others = (data?.enrollments ?? [])
    .filter(
      (enrollment: NodeSshEnrollmentSummary) => enrollment.id !== current
    )
    .slice(0, 5)
  if (others.length === 0) return null

  return (
    <div className="space-y-1 rounded-md border bg-background p-3">
      <p className="text-xs font-medium text-foreground">
        Recently added over SSH
      </p>
      {others.map((enrollment) => {
        const { label, tone } = enrollmentProgress(enrollment)
        return (
          <button
            key={enrollment.id}
            type="button"
            className="flex w-full items-center gap-2 text-left text-xs hover:underline"
            onClick={() => onSelect(enrollment.id)}
          >
            <span className="flex-1 truncate">
              <span className="font-medium text-foreground">
                {enrollment.name}
              </span>{' '}
              <span className="font-mono text-muted-foreground">
                {enrollment.ssh_address}
              </span>
            </span>
            <Badge
              variant="default"
              className={`${TONE_CLASSES[tone]} text-xs`}
            >
              {label}
            </Badge>
          </button>
        )
      })}
    </div>
  )
}
