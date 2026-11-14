// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * An in-memory Temps: the orchestrator side of the executor protocol, for
 * tests. It signs real invocation tokens, keeps runs and journals, applies
 * the same journal rules as the engine (fencing by lease, exactly-once
 * recording, sequence order, incompatibility) and the same outcome and event
 * semantics, on a virtual clock that tests advance.
 *
 * It talks to the workflow handler over the Fetch API, and serves the API the
 * handler and `WorkflowsClient` call through {@link FakeOrchestrator.fetch},
 * so the code under test is exactly what runs in production.
 */

import { createHash, generateKeyPairSync, randomUUID, sign, type KeyObject } from 'node:crypto';

import type { FetchHandler } from '../handler.js';
import {
  CHILD_EVENT,
  INVOKE_PATH,
  RESERVED_EVENT_PREFIX,
  MANIFEST_PATH,
  PROTOCOL_VERSION,
  TOKEN_VERSION,
  type ChildResult,
  type InvocationClaims,
  type InvocationKey,
  type InvokeRequest,
  type JournalEntry,
  type JsonValue,
  type Manifest,
  type Outcome,
  type RunStatus,
  type StepWriteRequest,
} from '../protocol.js';
import { verifyToken } from '../token.js';
import type { FetchLike } from '../transport.js';

export const FAKE_API_URL = 'http://temps.test/api';
export const FAKE_EXECUTOR_URL = 'http://executor.test';
export const FAKE_CREDENTIAL = 'twf_fake_credential_for_tests';

export interface FakeRun {
  id: string;
  workflow: string;
  input: JsonValue | null;
  status: RunStatus;
  dueAt: number | null;
  lease: number;
  attempt: number;
  journal: JournalEntry[];
  output: JsonValue | null;
  error: string | null;
  errorKind: string | null;
  idempotencyKey: string | null;
  createdAt: number;
  finishedAt: number | null;
  consecutiveFailures: number;
  cancelRequested: boolean;
  /** The run whose `child` entry started this one. */
  parentRunId: string | null;
  wait?: { sequence: number; event: string; key: string };
}

interface FakeEvent {
  id: number;
  name: string;
  key: string;
  payload: JsonValue | null;
  createdAt: number;
  consumedBy: Set<string>;
}

export interface FakeOrchestratorOptions {
  /** The executor under test. Set later with {@link FakeOrchestrator.attach}. */
  executor?: FetchHandler;
  /** Journal entries per invoke request before paging; default 1000. */
  journalPageSize?: number;
  leaseMs?: number;
  deadlineMs?: number;
  stuckAfterFailures?: number;
}

class HttpError extends Error {
  constructor(
    public readonly status: number,
    public readonly title: string,
    detail: string,
  ) {
    super(detail);
  }
}

const RUNNABLE = new Set<RunStatus>(['queued', 'sleeping', 'retrying', 'waiting']);
const TERMINAL = new Set<RunStatus>(['completed', 'failed', 'cancelled']);

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
}

export class FakeOrchestrator {
  readonly runs = new Map<string, FakeRun>();
  private readonly events: FakeEvent[] = [];
  private nextEventId = 1;
  private executor?: FetchHandler;
  private readonly privateKey: KeyObject;
  private readonly publicKey: KeyObject;
  readonly invocationKey: InvocationKey;
  private readonly inFlight = new Set<Promise<void>>();
  /** Virtual clock, in ms. */
  now: number = Date.now();
  readonly options: Required<Omit<FakeOrchestratorOptions, 'executor'>>;
  /** Every invoke reply status, for assertions. */
  readonly invocations: { runId: string; lease: number; status: number }[] = [];

  constructor(options: FakeOrchestratorOptions = {}) {
    const { privateKey, publicKey } = generateKeyPairSync('ed25519');
    this.privateKey = privateKey;
    this.publicKey = publicKey;
    const jwk = publicKey.export({ format: 'jwk' }) as { x: string };
    this.invocationKey = { algorithm: 'ed25519', public_key: jwk.x, key_id: 'fake', project_id: 1, environment_id: 1 };
    this.executor = options.executor;
    this.options = {
      journalPageSize: options.journalPageSize ?? 1000,
      leaseMs: options.leaseMs ?? 120_000,
      deadlineMs: options.deadlineMs ?? 900_000,
      stuckAfterFailures: options.stuckAfterFailures ?? 8,
    };
  }

  attach(executor: FetchHandler): void {
    this.executor = executor;
  }

  /** Sign `claims` as Temps would. */
  signToken(claims: InvocationClaims): string {
    const payload = Buffer.from(JSON.stringify(claims)).toString('base64url');
    const signature = sign(null, Buffer.from(`${TOKEN_VERSION}.${payload}`, 'ascii'), this.privateKey);
    return `${TOKEN_VERSION}.${payload}.${signature.toString('base64url')}`;
  }

  // ---- what an app does ------------------------------------------------

  start(
    workflow: string,
    input: JsonValue | null = null,
    idempotencyKey?: string,
    parentRunId: string | null = null,
  ): { run: FakeRun; created: boolean } {
    if (idempotencyKey) {
      for (const run of this.runs.values()) {
        if (run.workflow === workflow && run.idempotencyKey === idempotencyKey) return { run, created: false };
      }
    }
    const run: FakeRun = {
      id: randomUUID(),
      workflow,
      input,
      status: 'queued',
      dueAt: this.now,
      lease: 0,
      attempt: 0,
      journal: [],
      output: null,
      error: null,
      errorKind: null,
      idempotencyKey: idempotencyKey ?? null,
      createdAt: this.now,
      finishedAt: null,
      consecutiveFailures: 0,
      cancelRequested: false,
      parentRunId,
    };
    this.runs.set(run.id, run);
    return { run, created: true };
  }

  sendEvent(name: string, key: string | null = null, payload: JsonValue | null = null): { id: number; delivered: number } {
    const event: FakeEvent = {
      id: this.nextEventId++,
      name,
      key: key ?? '',
      payload,
      createdAt: this.now,
      consumedBy: new Set(),
    };
    this.events.push(event);
    let delivered = 0;
    for (const run of this.runs.values()) {
      if (run.status === 'waiting' && run.wait && run.wait.event === name && run.wait.key === event.key) {
        if (this.completeWait(run, event)) delivered += 1;
      }
    }
    return { id: event.id, delivered };
  }

  // ---- what an operator does -------------------------------------------

  /** Cancel a run and, with it, its children and theirs. */
  cancel(runId: string): void {
    const run = this.require(runId);
    if (TERMINAL.has(run.status)) return;
    const tree = [run];
    for (let i = 0; i < tree.length; i += 1) {
      for (const child of this.runs.values()) {
        if (child.parentRunId === (tree[i] as FakeRun).id) tree.push(child);
      }
    }
    for (const member of tree) {
      if (TERMINAL.has(member.status)) continue;
      Object.assign(member, { status: 'cancelled', dueAt: null, finishedAt: this.now, wait: undefined });
    }
    this.childFinished(run);
  }

  /** Cut the journal at `fromSequence` and run again. */
  retryFrom(runId: string, fromSequence: number): void {
    const run = this.require(runId);
    run.journal = run.journal.filter((entry) => entry.sequence < fromSequence);
    Object.assign(run, { status: 'queued', dueAt: this.now, output: null, error: null, errorKind: null, finishedAt: null });
  }

  resume(runId: string): void {
    const run = this.require(runId);
    if (!['stuck', 'retrying', 'sleeping', 'queued'].includes(run.status)) return;
    for (const entry of run.journal) {
      if (entry.kind === 'sleep' && entry.status === 'pending') entry.wake_at = new Date(this.now).toISOString();
    }
    Object.assign(run, { status: 'queued', dueAt: this.now, consecutiveFailures: 0 });
  }

  /** The lease of a running run expires (a dead replica): it is due again. */
  expireLease(runId: string): void {
    const run = this.require(runId);
    if (run.status !== 'running') return;
    run.status = 'queued';
    run.dueAt = this.now;
  }

  // ---- the scheduler ---------------------------------------------------

  /** Start every due run's invocation without waiting for it. */
  kick(): number {
    let started = 0;
    for (const run of this.runs.values()) {
      if (!RUNNABLE.has(run.status) || run.dueAt === null || run.dueAt > this.now) continue;
      run.status = 'running';
      run.lease += 1;
      run.attempt += 1;
      run.dueAt = null;
      const task = this.invoke(run, run.lease).finally(() => this.inFlight.delete(task));
      this.inFlight.add(task);
      started += 1;
    }
    return started;
  }

  /** Run until nothing is due and nothing is in flight (the clock stands still). */
  async settle(maxRounds = 10_000): Promise<void> {
    for (let round = 0; round < maxRounds; round += 1) {
      this.kick();
      if (this.inFlight.size === 0) return;
      await Promise.race(this.inFlight);
    }
    throw new Error(`FakeOrchestrator.settle: still busy after ${maxRounds} rounds (a run retrying without end?)`);
  }

  /** Move the clock forward, then settle. */
  async advance(ms: number): Promise<void> {
    this.now += ms;
    await this.settle();
  }

  /** The executor's manifest, read with a manifest token. */
  async manifest(): Promise<Manifest> {
    const response = await this.callExecutor(MANIFEST_PATH, 'GET', undefined, {
      v: 1,
      typ: 'manifest',
      pid: 1,
      eid: 1,
      exp: Math.floor(Date.now() / 1000) + 60,
    });
    if (response.status !== 200) throw new Error(`manifest read failed with ${response.status}`);
    return (await response.json()) as Manifest;
  }

  private async callExecutor(
    path: string,
    method: string,
    body: Uint8Array | undefined,
    claims: InvocationClaims,
  ): Promise<Response> {
    if (!this.executor) throw new Error('FakeOrchestrator: no executor attached');
    return this.executor(
      new Request(`${FAKE_EXECUTOR_URL}${path}`, {
        method,
        headers: { authorization: `Bearer ${this.signToken(claims)}`, 'content-type': 'application/json' },
        body: body as BodyInit | undefined,
      }),
    );
  }

  private invokeRequest(run: FakeRun): { request: InvokeRequest; body: Uint8Array } {
    const page = run.journal.slice(0, this.options.journalPageSize);
    const request: InvokeRequest = {
      protocol_version: PROTOCOL_VERSION,
      run_id: run.id,
      workflow: run.workflow,
      attempt: run.attempt,
      lease: run.lease,
      now: new Date(this.now).toISOString(),
      deadline_at: new Date(this.now + this.options.deadlineMs).toISOString(),
      lease_expires_at: new Date(this.now + this.options.leaseMs).toISOString(),
      input: run.input,
      journal: structuredClone(page),
      journal_next_sequence: run.journal.length > page.length ? page.length : null,
    };
    return { request, body: new TextEncoder().encode(JSON.stringify(request)) };
  }

  private async invoke(run: FakeRun, lease: number): Promise<void> {
    const { body } = this.invokeRequest(run);
    const claims: InvocationClaims = {
      v: 1,
      typ: 'invoke',
      pid: 1,
      eid: 1,
      run: run.id,
      lease,
      exp: Math.floor(Date.now() / 1000) + Math.ceil(this.options.deadlineMs / 1000) + 60,
      body: createHash('sha256').update(body).digest('hex'),
    };
    let status = 0;
    try {
      const response = await this.callExecutor(INVOKE_PATH, 'POST', body, claims);
      status = response.status;
      if (status === 200) {
        const reply = (await response.json()) as { outcome: Outcome };
        if (this.holds(run, lease)) this.applyOutcome(run, reply.outcome);
      } else if (status !== 202) {
        this.invocationFailed(run, lease);
      }
    } catch {
      this.invocationFailed(run, lease);
    } finally {
      this.invocations.push({ runId: run.id, lease, status });
    }
  }

  private invocationFailed(run: FakeRun, lease: number): void {
    if (!this.holds(run, lease)) return;
    run.consecutiveFailures += 1;
    if (run.consecutiveFailures >= this.options.stuckAfterFailures) {
      run.status = 'stuck';
      run.dueAt = null;
      return;
    }
    run.status = 'retrying';
    run.dueAt = this.now + Math.min(300, 5 * 2 ** Math.min(run.consecutiveFailures - 1, 6)) * 1000;
  }

  private holds(run: FakeRun, lease: number): boolean {
    return run.status === 'running' && run.lease === lease;
  }

  private requirePending(run: FakeRun, sequence: number, kind: JournalEntry['kind']): void {
    const entry = run.journal[sequence];
    if (!entry || entry.kind !== kind || entry.status !== 'pending') {
      throw new HttpError(
        400,
        'Validation Error',
        `run ${run.id} suspended on entry ${sequence}, which is not a pending ${kind} entry; record it first`,
      );
    }
  }

  private finish(run: FakeRun, status: RunStatus, fields: Partial<FakeRun>): void {
    Object.assign(run, { status, dueAt: null, finishedAt: this.now, consecutiveFailures: 0, ...fields });
    this.childFinished(run);
  }

  /** A finished child wakes its parent, if the parent is suspended on it. */
  private childFinished(child: FakeRun): void {
    const parent = child.parentRunId ? this.runs.get(child.parentRunId) : undefined;
    if (!parent || parent.status !== 'waiting' || parent.wait?.event !== CHILD_EVENT || parent.wait.key !== child.id) return;
    this.completeChild(parent, child);
  }

  private completeChild(parent: FakeRun, child: FakeRun | undefined): void {
    if (!parent.wait) return;
    const childId = parent.wait.key;
    const result: ChildResult = child
      ? {
          run_id: child.id,
          status: child.status as ChildResult['status'],
          output: child.status === 'completed' ? child.output : null,
          error: child.error ?? (child.status === 'cancelled' ? `child run ${child.id} was cancelled` : null),
        }
      : { run_id: childId, status: 'deleted', output: null, error: `child run ${childId} was deleted before it finished` };
    const entry = parent.journal[parent.wait.sequence];
    if (!entry || entry.kind !== 'child' || entry.status !== 'pending') return;
    entry.status = 'completed';
    entry.result = result as unknown as JsonValue;
    Object.assign(parent, { wait: undefined, status: 'queued', dueAt: this.now });
  }

  private applyOutcome(run: FakeRun, outcome: Outcome): void {
    run.consecutiveFailures = 0;
    switch (outcome.type) {
      case 'completed':
        this.finish(run, 'completed', { output: outcome.output ?? null });
        return;
      case 'failed':
        this.finish(run, 'failed', {
          error: outcome.error,
          errorKind: outcome.sequence === undefined || outcome.sequence === null ? 'workflow_failed' : 'step_failed',
        });
        return;
      case 'sleep':
        this.requirePending(run, outcome.sequence, 'sleep');
        Object.assign(run, { status: 'sleeping', dueAt: Date.parse(outcome.until) });
        return;
      case 'wait': {
        const key = outcome.key ?? '';
        if (outcome.event === CHILD_EVENT) {
          this.requirePending(run, outcome.sequence, 'child');
          const child = this.runs.get(key);
          if (!child || child.parentRunId !== run.id || outcome.timeout_at) {
            throw new HttpError(400, 'Validation Error', `run ${run.id} suspended on child '${key}', which is not the run its entry ${outcome.sequence} started`);
          }
          Object.assign(run, { status: 'waiting', dueAt: null, wait: { sequence: outcome.sequence, event: CHILD_EVENT, key } });
          if (TERMINAL.has(child.status)) this.completeChild(run, child);
          return;
        }
        if (outcome.event.startsWith(RESERVED_EVENT_PREFIX)) {
          throw new HttpError(400, 'Validation Error', `run ${run.id} waits on '${outcome.event}', which is reserved for events Temps sends itself`);
        }
        this.requirePending(run, outcome.sequence, 'wait');
        run.status = 'waiting';
        run.dueAt = outcome.timeout_at ? Date.parse(outcome.timeout_at) : null;
        run.wait = { sequence: outcome.sequence, event: outcome.event, key };
        const buffered = this.events.find(
          (event) =>
            event.name === outcome.event && event.key === key && event.createdAt >= run.createdAt && !event.consumedBy.has(run.id),
        );
        if (buffered) this.completeWait(run, buffered);
        return;
      }
      case 'retry':
        Object.assign(run, { status: 'retrying', dueAt: Math.max(Date.parse(outcome.at), this.now) });
        return;
      case 'incompatible':
        this.finish(run, 'failed', {
          errorKind: 'incompatible',
          error: `WorkflowIncompatible { run_id: ${run.id}, step: ${outcome.sequence}, expected: ${JSON.stringify(outcome.expected)}, found: ${JSON.stringify(outcome.found)} }`,
        });
        return;
    }
  }

  private completeWait(run: FakeRun, event: FakeEvent): boolean {
    if (run.status !== 'waiting' || !run.wait) return false;
    const entry = run.journal[run.wait.sequence];
    if (!entry || entry.status !== 'pending') return false;
    entry.status = 'completed';
    entry.result = { received: true, payload: event.payload };
    event.consumedBy.add(run.id);
    run.wait = undefined;
    run.status = 'queued';
    run.dueAt = this.now;
    return true;
  }

  private require(runId: string): FakeRun {
    const run = this.runs.get(runId);
    if (!run) throw new HttpError(404, 'Workflow Run Not Found', `Workflow run ${runId} was not found`);
    return run;
  }

  // ---- the API the executor and the app call ----------------------------

  /** A `fetch` for the handler's transport and `WorkflowsClient`. */
  readonly fetch: FetchLike = async (input, init) => {
    try {
      return await this.route(new Request(input, init));
    } catch (error) {
      if (error instanceof HttpError) {
        return new Response(JSON.stringify({ status: error.status, title: error.title, detail: error.message }), {
          status: error.status,
          headers: { 'content-type': 'application/problem+json' },
        });
      }
      throw error;
    }
  };

  private authorize(request: Request, runId: string): { run: FakeRun; lease: number } {
    const token = (request.headers.get('authorization') ?? '').replace(/^Bearer\s+/i, '');
    let claims: InvocationClaims;
    try {
      claims = verifyToken(token, this.publicKey);
    } catch (error) {
      throw new HttpError(401, 'Invalid Invocation Token', error instanceof Error ? error.message : 'invalid');
    }
    if (claims.typ !== 'invoke' || claims.run !== runId || claims.lease === undefined) {
      throw new HttpError(401, 'Invalid Invocation Token', 'token is for another run');
    }
    return { run: this.require(runId), lease: claims.lease };
  }

  private readonly admitted = new Map<string, number>();

  private requireLease(run: FakeRun, lease: number): void {
    if (!this.holds(run, lease)) {
      throw new HttpError(
        409,
        'Stale Lease',
        `Run ${run.id} is not held by lease ${lease} (current lease ${run.lease}, status ${run.status})`,
      );
    }
  }

  private async route(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const path = url.pathname.replace(/^\/api/, '');
    const body = request.method === 'POST' ? ((await request.json()) as Record<string, unknown>) : {};
    let match: RegExpExecArray | null;

    if (path === '/workflows/v1/invocation-key') {
      if (request.headers.get('authorization') !== `Bearer ${FAKE_CREDENTIAL}`) {
        throw new HttpError(401, 'Invalid Workflow Credential', 'the invocation key is read with the workflow credential');
      }
      return json(200, this.invocationKey);
    }

    if ((match = /^\/workflow-runs\/([^/]+)\/admit$/.exec(path))) {
      const { run, lease } = this.authorize(request, match[1] as string);
      this.requireLease(run, lease);
      if (this.admitted.get(run.id) === lease) throw new HttpError(409, 'Stale Lease', 'invocation already admitted');
      this.admitted.set(run.id, lease);
      const entries = run.journal.slice(0, this.options.journalPageSize);
      return json(200, { entries: structuredClone(entries), next_sequence: entries.length < run.journal.length ? entries.length : null });
    }
    if ((match = /^\/workflow-runs\/([^/]+)\/journal$/.exec(path))) {
      const { run, lease } = this.authorize(request, match[1] as string);
      this.requireLease(run, lease);
      const from = Number(url.searchParams.get('from') ?? '0');
      const entries = run.journal.slice(from, from + this.options.journalPageSize);
      const next = from + entries.length < run.journal.length ? from + entries.length : null;
      return json(200, { entries: structuredClone(entries), next_sequence: next });
    }
    if ((match = /^\/workflow-runs\/([^/]+)\/steps$/.exec(path))) {
      const { run, lease } = this.authorize(request, match[1] as string);
      return json(200, this.recordStep(run, lease, body as unknown as StepWriteRequest));
    }
    if ((match = /^\/workflow-runs\/([^/]+)\/heartbeat$/.exec(path))) {
      const { run, lease } = this.authorize(request, match[1] as string);
      if (run.status === 'cancelled') return json(200, { lease_expires_at: new Date(this.now).toISOString(), cancelled: true });
      this.requireLease(run, lease);
      return json(200, { lease_expires_at: new Date(this.now + this.options.leaseMs).toISOString(), cancelled: false });
    }
    if ((match = /^\/workflow-runs\/([^/]+)\/outcome$/.exec(path))) {
      const { run, lease } = this.authorize(request, match[1] as string);
      this.requireLease(run, lease);
      this.applyOutcome(run, (body as { outcome: Outcome }).outcome);
      return json(200, { status: run.status });
    }

    // App API, with the workflow credential.
    if (request.headers.get('authorization') !== `Bearer ${FAKE_CREDENTIAL}`) {
      throw new HttpError(401, 'Invalid Workflow Credential', 'unknown credential');
    }
    if (path === '/workflows/v1/runs' && request.method === 'POST') {
      const { run, created } = this.start(
        body.workflow as string,
        (body.input ?? null) as JsonValue,
        (body.idempotency_key as string | null) ?? undefined,
      );
      return json(created ? 201 : 200, { run: this.view(run), created });
    }
    if ((match = /^\/workflows\/v1\/runs\/([^/]+)$/.exec(path))) {
      return json(200, this.view(this.require(match[1] as string)));
    }
    if (path === '/workflows/v1/events' && request.method === 'POST') {
      const { id, delivered } = this.sendEvent(
        body.name as string,
        (body.key as string | null) ?? null,
        (body.payload ?? null) as JsonValue,
      );
      return json(200, { event_id: id, delivered });
    }
    throw new HttpError(404, 'Not Found', `${request.method} ${path}`);
  }

  view(run: FakeRun) {
    return {
      id: run.id,
      workflow: run.workflow,
      status: run.status,
      error_kind: run.errorKind,
      output: run.output,
      error: run.error,
      created_at: new Date(run.createdAt).toISOString(),
      finished_at: run.finishedAt === null ? null : new Date(run.finishedAt).toISOString(),
    };
  }

  private recordStep(run: FakeRun, lease: number, write: StepWriteRequest) {
    this.requireLease(run, lease);
    const invalid = (reason: string) => new HttpError(400, 'Validation Error', reason);
    if (write.sequence < 0) throw invalid('sequence must be zero or more');
    if (write.attempts < 1) throw invalid('attempts must be at least 1');
    if (write.kind === 'sleep' && write.status === 'pending' && !write.wake_at) {
      throw invalid(`pending sleep ${write.sequence} of run ${run.id} needs wake_at`);
    }
    if (['now', 'random', 'send_event'].includes(write.kind) && write.status === 'pending') {
      throw invalid(`a ${write.kind} entry is never pending`);
    }
    if ((write.kind === 'send_event') !== Boolean(write.event)) {
      throw invalid('only a send_event entry carries an event, and it must');
    }
    if (write.event?.name.startsWith(RESERVED_EVENT_PREFIX)) {
      throw invalid(`event name '${write.event.name}' starts with '${RESERVED_EVENT_PREFIX}', which is reserved for events Temps sends itself`);
    }
    if (write.kind === 'child') {
      if (write.status !== 'pending' || !write.child || write.result) {
        throw invalid(`child ${write.sequence} of run ${run.id} is recorded pending with \`child\` and no result; Temps completes it`);
      }
      const { run: child } = this.start(write.child.workflow, write.child.input ?? null, `child:${run.id}:${write.sequence}`, run.id);
      write = { ...write, result: { run_id: child.id } };
    } else if (write.child) {
      throw invalid(`only a child entry carries \`child\`, not ${write.kind}`);
    }
    const leaseExpiresAt = new Date(this.now + this.options.leaseMs).toISOString();
    const existing = run.journal[write.sequence];
    if (existing) {
      if (existing.name !== write.name || existing.kind !== write.kind) {
        throw new HttpError(
          409,
          'Workflow Incompatible',
          `WorkflowIncompatible { run_id: ${run.id}, step: ${write.sequence}, expected: ${JSON.stringify(existing.name)}, found: ${JSON.stringify(write.name)} }`,
        );
      }
      if (existing.status !== 'pending') {
        return { entry: structuredClone(existing), already_recorded: true, lease_expires_at: leaseExpiresAt };
      }
      Object.assign(existing, {
        status: write.status,
        attempts: write.attempts,
        result: write.result ?? null,
        error: write.error ?? null,
        wake_at: write.wake_at ?? existing.wake_at ?? null,
      });
      return { entry: structuredClone(existing), already_recorded: false, lease_expires_at: leaseExpiresAt };
    }
    if (write.sequence !== run.journal.length) {
      throw invalid(`entry ${write.sequence} of run ${run.id} is not the next one; the journal has ${run.journal.length} entries`);
    }
    const entry: JournalEntry = {
      sequence: write.sequence,
      name: write.name,
      kind: write.kind,
      status: write.status,
      attempts: write.attempts,
      result: write.result ?? null,
      error: write.error ?? null,
      wake_at: write.wake_at ?? null,
    };
    run.journal.push(entry);
    if (write.event) this.sendEvent(write.event.name, write.event.key ?? null, write.event.payload ?? null);
    return { entry: structuredClone(entry), already_recorded: false, lease_expires_at: leaseExpiresAt };
  }
}
