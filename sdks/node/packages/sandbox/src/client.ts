import { SandboxError } from './errors.js';
import { request, resolveConfig, type ResolvedConfig } from './http.js';
import type {
  CreateSandboxOptions,
  ExecOptions,
  ExecResult,
  JobHandle,
  JobState,
  ListSandboxesOptions,
  ListSandboxesPage,
  SandboxConfig,
  SandboxEvent,
  SandboxSource,
  SandboxSummary,
  StatInfo,
  WriteFileOptions,
} from './types.js';

// Wire shapes. Separate from the public `types.ts` so the SDK can adopt
// idiomatic camelCase at the boundary without the generated types
// leaking snake_case into user code.

// `SandboxInner` on the server (`@vercel/sandbox`-shaped): camelCase core
// fields, `cwd` for the work dir, epoch-ms timestamps, `timeout` in ms.
interface WireSandboxInner {
  id: string;
  name: string;
  status: string;
  image: string | null;
  cwd: string;
  /** Creation time, Unix epoch milliseconds. */
  createdAt: number;
  /** Idle timeout in milliseconds. */
  timeout: number;
  backend?: string | null;
  disk_size_mb?: number | null;
  preview_url_template: string;
  preview_password_hint?: string;
}

/** Create/get responses wrap the sandbox with its preview routes. */
interface WireSandboxEnvelope {
  sandbox: WireSandboxInner;
  routes?: unknown[];
}

interface WireListSandboxes {
  sandboxes: WireSandboxInner[];
  pagination: { count: number; next: string | null; prev: string | null };
}

interface WireExecResult {
  exit_code: number;
  stdout: string;
  stderr: string;
}

interface WireJobStatus {
  status: 'running' | 'exited' | 'failed';
  exit_code: number | null;
  reason: string | null;
  stdout: string;
  stderr: string;
}

interface WireStat {
  path: string;
  exists: boolean;
  is_dir: boolean;
  is_file: boolean;
  size: number;
}

function toSummary(w: WireSandboxInner): SandboxSummary {
  return {
    id: w.id,
    name: w.name,
    status: w.status,
    image: w.image,
    workDir: w.cwd,
    backend: w.backend ?? undefined,
    diskSizeMb: w.disk_size_mb ?? undefined,
    createdAt: new Date(w.createdAt).toISOString(),
    expiresAt: new Date(w.createdAt + w.timeout).toISOString(),
    previewUrlTemplate: w.preview_url_template,
    previewPasswordHint: w.preview_password_hint,
  };
}

function toCreateBody(opts: CreateSandboxOptions): Record<string, unknown> {
  return {
    name: opts.name,
    image: opts.image,
    timeout_secs: opts.timeoutSecs,
    env: opts.env,
    cpu_limit: opts.cpuLimit,
    memory_limit_mb: opts.memoryLimitMb,
    pids_limit: opts.pidsLimit,
    source: opts.source ? toSourceBody(opts.source) : undefined,
    preview_password: opts.previewPassword,
    backend: opts.backend,
  };
}

function toSourceBody(src: SandboxSource): Record<string, unknown> {
  if (src.type === 'git') {
    return {
      type: 'git',
      url: src.url,
      revision: src.revision,
      depth: src.depth,
      username: src.username,
      password: src.password,
      git_connection_id: src.gitConnectionId,
    };
  }
  return { type: 'tarball', url: src.url };
}

function toBase64(contents: string | Uint8Array): string {
  const bytes =
    typeof contents === 'string'
      ? new TextEncoder().encode(contents)
      : contents;
  if (typeof Buffer !== 'undefined') return Buffer.from(bytes).toString('base64');
  // Browser fallback
  let binary = '';
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary);
}

function fromBase64(b64: string): Uint8Array {
  if (typeof Buffer !== 'undefined')
    return new Uint8Array(Buffer.from(b64, 'base64'));
  const binary = atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

/**
 * A live sandbox. Instances are returned by {@link Sandbox.create} and
 * {@link Sandbox.get}; they hold both the server-assigned ID and the
 * client config needed to make follow-up calls.
 *
 * The shape intentionally mirrors `@vercel/sandbox` — switching providers
 * should require only swapping the import and base URL.
 */
export class Sandbox {
  readonly id: string;
  private summary: SandboxSummary;
  private readonly cfg: ResolvedConfig;

  private constructor(summary: SandboxSummary, cfg: ResolvedConfig) {
    this.id = summary.id;
    this.summary = summary;
    this.cfg = cfg;
  }

  /** Current metadata snapshot. Call {@link refresh} to reload from the server. */
  get info(): SandboxSummary {
    return this.summary;
  }

  /** Provision a new sandbox. */
  static async create(
    config: SandboxConfig & CreateSandboxOptions = {}
  ): Promise<Sandbox> {
    const { apiUrl, apiToken, fetch, ...opts } = config;
    const cfg = resolveConfig({ apiUrl, apiToken, fetch });
    const wire = await request<WireSandboxEnvelope>(
      cfg,
      'POST',
      '/v1/sandboxes',
      toCreateBody(opts)
    );
    return new Sandbox(toSummary(wire.sandbox), cfg);
  }

  /** Fetch an existing sandbox by ID. */
  static async get(id: string, config: SandboxConfig = {}): Promise<Sandbox> {
    const cfg = resolveConfig(config);
    const wire = await request<WireSandboxEnvelope>(
      cfg,
      'GET',
      `/v1/sandboxes/${id}`
    );
    return new Sandbox(toSummary(wire.sandbox), cfg);
  }

  /** Page through the caller's sandboxes. */
  static async list(
    config: SandboxConfig & ListSandboxesOptions = {}
  ): Promise<ListSandboxesPage> {
    const { apiUrl, apiToken, fetch, page, pageSize } = config;
    const cfg = resolveConfig({ apiUrl, apiToken, fetch });
    const wire = await request<WireListSandboxes>(
      cfg,
      'GET',
      '/v1/sandboxes',
      undefined,
      { page, page_size: pageSize }
    );
    return {
      items: wire.sandboxes.map(toSummary),
      total: wire.pagination.count,
      page: page ?? 1,
      pageSize: pageSize ?? 20,
    };
  }

  // ── Instance methods ──

  async refresh(): Promise<SandboxSummary> {
    const wire = await request<WireSandboxEnvelope>(
      this.cfg,
      'GET',
      `/v1/sandboxes/${this.id}`
    );
    this.summary = toSummary(wire.sandbox);
    return this.summary;
  }

  /**
   * Run a command synchronously. Blocks until the process exits; stdout
   * and stderr are buffered and returned together. For long-running
   * processes use {@link execDetached} and stream the job instead.
   */
  async exec(cmd: string[], options: Omit<ExecOptions, 'cmd'> = {}): Promise<ExecResult> {
    const wire = await request<WireExecResult>(
      this.cfg,
      'POST',
      `/v1/sandboxes/${this.id}/exec`,
      { cmd, env: options.env, cwd: options.cwd }
    );
    return {
      exitCode: wire.exit_code,
      stdout: wire.stdout,
      stderr: wire.stderr,
    };
  }

  /** Start a background job. Returns a handle — poll with {@link jobStatus}. */
  async execDetached(
    cmd: string[],
    options: Omit<ExecOptions, 'cmd'> = {}
  ): Promise<JobHandle> {
    const wire = await request<{ job_id: string }>(
      this.cfg,
      'POST',
      `/v1/sandboxes/${this.id}/exec-detached`,
      { cmd, env: options.env, cwd: options.cwd }
    );
    return { jobId: wire.job_id };
  }

  async jobStatus(jobId: string): Promise<JobState> {
    const wire = await request<WireJobStatus>(
      this.cfg,
      'GET',
      `/v1/sandboxes/${this.id}/jobs/${jobId}`
    );
    return {
      status: wire.status,
      exitCode: wire.exit_code,
      reason: wire.reason,
      stdout: wire.stdout,
      stderr: wire.stderr,
    };
  }

  async killJob(jobId: string, signal?: string): Promise<void> {
    await request<void>(
      this.cfg,
      'POST',
      `/v1/sandboxes/${this.id}/jobs/${jobId}/kill`,
      signal ? { signal } : undefined
    );
  }

  async writeFile(opts: WriteFileOptions): Promise<void> {
    await request<void>(this.cfg, 'POST', `/v1/sandboxes/${this.id}/fs/write`, {
      path: opts.path,
      contents_b64: toBase64(opts.contents),
      mode: opts.mode,
    });
  }

  async readFile(path: string): Promise<Uint8Array> {
    const wire = await request<{ path: string; contents_b64: string; size: number }>(
      this.cfg,
      'GET',
      `/v1/sandboxes/${this.id}/fs/read`,
      undefined,
      { path }
    );
    return fromBase64(wire.contents_b64);
  }

  async mkdir(path: string): Promise<void> {
    await request<void>(this.cfg, 'POST', `/v1/sandboxes/${this.id}/fs/mkdir`, {
      path,
    });
  }

  async stat(path: string): Promise<StatInfo> {
    const wire = await request<WireStat>(
      this.cfg,
      'GET',
      `/v1/sandboxes/${this.id}/fs/stat`,
      undefined,
      { path }
    );
    return {
      path: wire.path,
      exists: wire.exists,
      isDir: wire.is_dir,
      isFile: wire.is_file,
      size: wire.size,
    };
  }

  /**
   * Build a public preview URL for a port exposed inside the sandbox.
   * Returns `null` if this install doesn't have preview URLs configured
   * (the template is empty). Matches `@vercel/sandbox`'s `domain(port)`.
   */
  domain(port: number): string | null {
    const tpl = this.summary.previewUrlTemplate;
    if (!tpl) return null;
    return tpl.replace('{port}', String(port));
  }

  async extendTimeout(extraSecs: number): Promise<void> {
    await request<void>(
      this.cfg,
      'POST',
      `/v1/sandboxes/${this.id}/extend-timeout`,
      { extra_secs: extraSecs }
    );
  }

  async pause(): Promise<void> {
    await request<void>(this.cfg, 'POST', `/v1/sandboxes/${this.id}/pause`);
  }

  async resume(): Promise<void> {
    await request<void>(this.cfg, 'POST', `/v1/sandboxes/${this.id}/resume`);
  }

  async restart(): Promise<void> {
    await request<void>(this.cfg, 'POST', `/v1/sandboxes/${this.id}/restart`);
  }

  /** Stop the container but keep the row (can `resume` later). */
  async stop(): Promise<void> {
    await request<void>(this.cfg, 'POST', `/v1/sandboxes/${this.id}/stop`);
  }

  /** Stop **and** delete the sandbox. Irreversible. */
  async destroy(): Promise<void> {
    await request<void>(this.cfg, 'POST', `/v1/sandboxes/${this.id}/destroy`);
  }

  /**
   * Grow the root disk to `diskSizeMb` (Firecracker only; grow-only). The
   * sandbox restarts briefly to apply it — the filesystem is preserved.
   * Returns the refreshed summary.
   */
  async resize(diskSizeMb: number): Promise<SandboxSummary> {
    const wire = await request<WireSandboxEnvelope>(
      this.cfg,
      'POST',
      `/v1/sandboxes/${this.id}/resize`,
      { disk_size_mb: diskSizeMb }
    );
    this.summary = toSummary(wire.sandbox);
    return this.summary;
  }

  /**
   * The operations timeline for this sandbox (lifecycle events only —
   * create/stop/resume/restart/extend/resize/preview-password/destroy —
   * never shell activity), newest first.
   */
  async events(limit?: number): Promise<SandboxEvent[]> {
    const wire = await request<{ events: SandboxEvent[] }>(
      this.cfg,
      'GET',
      `/v1/sandboxes/${this.id}/events`,
      undefined,
      limit ? { limit } : undefined
    );
    return wire.events;
  }
}

export { SandboxError };
