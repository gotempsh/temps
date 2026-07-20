// Public types. Names and shapes mirror the HTTP request/response DTOs
// defined in `temps-sandbox::handlers::sandboxes`. Keep this file in
// lock-step with the Rust definitions — clients rely on these being the
// wire format, not a convenience projection.

export interface SandboxConfig {
  /** Base URL of the Temps control plane (e.g. https://temps.example.com). */
  apiUrl?: string;
  /** Personal access token: `temps_pat_...`. */
  apiToken?: string;
  /** Optional fetch override (tests, custom transports). */
  fetch?: typeof fetch;
}

export type SandboxSource =
  | {
      type: 'git';
      url: string;
      revision?: string;
      depth?: number;
      username?: string;
      password?: string;
      gitConnectionId?: number;
    }
  | {
      type: 'tarball';
      url: string;
    };

export interface CreateSandboxOptions {
  /** Friendly label surfaced in the UI and `list` output. */
  name?: string;
  /** Docker image override. Omit to use the platform default. */
  image?: string;
  /** Idle timeout in seconds. Clamped server-side to [60, 86400]. */
  timeoutSecs?: number;
  /** Extra env vars baked into the container on create. */
  env?: Record<string, string>;
  cpuLimit?: number;
  memoryLimitMb?: number;
  pidsLimit?: number;
  /** Seed initial contents from a git repo or tarball. */
  source?: SandboxSource;
  /** Optional plaintext password for preview URLs (8–256 chars). */
  previewPassword?: string;
  /**
   * Isolation backend: 'docker' (default) or 'firecracker' (hardware-
   * virtualized microVM; requires a host provisioned with
   * `temps firecracker setup`). Requesting an unavailable backend fails
   * rather than silently downgrading isolation.
   */
  backend?: 'docker' | 'firecracker';
}

export interface SandboxSummary {
  id: string;
  name: string;
  status: string;
  image: string | null;
  workDir: string;
  /** Isolation backend the sandbox runs on ('docker' | 'firecracker'). */
  backend?: string;
  /** Root disk size in MB (Firecracker). */
  diskSizeMb?: number;
  createdAt: string;
  expiresAt: string;
  /**
   * URL template with a literal `{port}` placeholder. Use `sandbox.domain(port)`
   * to get a concrete URL rather than reading this directly.
   */
  previewUrlTemplate: string;
  previewPasswordHint?: string;
}

/** One entry in a sandbox's operations timeline. */
export interface SandboxEvent {
  /**
   * Machine-readable operation: 'created' | 'stopped' | 'resumed' |
   * 'restarted' | 'timeout_extended' | 'resized' | 'preview_password_set' |
   * 'preview_password_cleared' | 'source_seeded' | 'destroyed'.
   */
  event_type: string;
  /** Optional structured context; shape depends on `event_type`. */
  detail?: Record<string, unknown> | null;
  /** Event time, Unix epoch milliseconds. */
  at: number;
}

export interface ExecOptions {
  /** Argv array. First element is the program; the rest are args. */
  cmd: string[];
  env?: Record<string, string>;
  cwd?: string;
}

export interface ExecResult {
  exitCode: number;
  stdout: string;
  stderr: string;
}

export interface JobHandle {
  /** Opaque job ID used to poll status or stream logs. */
  jobId: string;
}

export type JobStatus = 'running' | 'exited' | 'failed';

export interface JobState {
  status: JobStatus;
  exitCode: number | null;
  reason: string | null;
  stdout: string;
  stderr: string;
}

export interface WriteFileOptions {
  /** Absolute path inside the sandbox (must start with `/`). */
  path: string;
  /** File contents. Strings are UTF-8 encoded; Uint8Array is used as-is. */
  contents: string | Uint8Array;
  /** Unix mode (e.g. 0o755). Defaults to 0o644. */
  mode?: number;
}

export interface StatInfo {
  path: string;
  exists: boolean;
  isDir: boolean;
  isFile: boolean;
  size: number;
}

export interface ListSandboxesPage {
  items: SandboxSummary[];
  total: number;
  page: number;
  pageSize: number;
}

export interface ListSandboxesOptions {
  page?: number;
  pageSize?: number;
}
