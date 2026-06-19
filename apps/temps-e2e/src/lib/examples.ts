/**
 * Registry of source-based example projects (from the repo's `examples/` tree)
 * and a helper to build each into a local Docker image the e2e scenario can
 * deploy.
 *
 * The examples ship as source only (no Dockerfile), so each entry carries a
 * minimal Dockerfile rendered into a scratch build context — the repo tree is
 * never mutated. The built image is tagged `temps-e2e-example/<key>:latest` and
 * deployed through the same image path the `scenario` command already uses, so
 * "test other project types" is just "build the example, then run the normal
 * deploy → load → verify → teardown lifecycle".
 *
 * Only HTTP-serving examples are listed: an example must expose an HTTP port and
 * (ideally) a health route so the deploy can be verified end-to-end. The
 * `node/vercel-ai-tracing` example is intentionally absent — it is a one-shot
 * OTel tracing script, not a server, and needs external LLM API keys.
 */
import { mkdir, rm, cp, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

export interface ExampleProject {
  /** Stable key used on the CLI (`--only`) and in the image tag. */
  key: string
  /** Human label for output. */
  label: string
  /** Path of the example relative to the repo's `examples/` dir. */
  relDir: string
  /** Container port the app listens on (also passed to the app as $PORT). */
  port: number
  /** HTTP path that should return 2xx once the app is up. */
  healthPath: string
  /** Minimal Dockerfile rendered into the scratch build context. */
  dockerfile: string
  /** Rough build-cost hint so callers can pick a fast subset. */
  weight: 'light' | 'medium' | 'heavy'
}

/**
 * The example registry. Dockerfiles are deliberately minimal and self-contained
 * (no reliance on a lockfile-specific toolchain) so they build from a clean
 * checkout. PORT is injected as an env var because every example reads it.
 */
export const EXAMPLES: ExampleProject[] = [
  {
    key: 'go-gin',
    label: 'Go (Gin)',
    relDir: 'go/gin-basic',
    port: 3000,
    healthPath: '/health',
    weight: 'light',
    dockerfile: `FROM golang:1.24-alpine AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 GOOS=linux go build -o /server main.go

FROM gcr.io/distroless/static-debian12
COPY --from=build /server /server
ENV PORT=3000
EXPOSE 3000
ENTRYPOINT ["/server"]
`,
  },
  {
    key: 'python-flask',
    label: 'Python (Flask + gunicorn)',
    relDir: 'python/flask-basic',
    port: 3000,
    healthPath: '/health',
    weight: 'light',
    dockerfile: `FROM python:3.12-slim
WORKDIR /app
COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt
COPY . .
ENV PORT=3000
EXPOSE 3000
# main.py exposes a Flask app named "app"; serve it with gunicorn.
CMD ["sh", "-c", "gunicorn -w 2 -b 0.0.0.0:\${PORT} main:app"]
`,
  },
  {
    key: 'node-nestjs',
    label: 'Node (NestJS)',
    relDir: 'nestjs/basic',
    port: 3000,
    healthPath: '/',
    weight: 'medium',
    dockerfile: `FROM node:22-slim AS build
WORKDIR /app
COPY package.json ./
# No lockfile coupling: install then build the Nest dist.
RUN npm install
COPY . .
RUN npm run build

FROM node:22-slim
WORKDIR /app
ENV NODE_ENV=production
COPY package.json ./
RUN npm install --omit=dev
COPY --from=build /app/dist ./dist
ENV PORT=3000
EXPOSE 3000
CMD ["node", "dist/main"]
`,
  },
  {
    key: 'vite-react',
    label: 'Vite (React, static via nginx)',
    relDir: 'vite/react-basic',
    // nginx-unprivileged listens on 8080 and runs rootless — required because
    // Temps runs containers as a non-root user, which makes the stock nginx
    // image crash-loop ("chown(/var/cache/nginx/...) Operation not permitted").
    port: 8080,
    healthPath: '/',
    weight: 'medium',
    dockerfile: `FROM node:22-slim AS build
WORKDIR /app
COPY package.json ./
RUN npm install
COPY . .
RUN npm run build

FROM nginxinc/nginx-unprivileged:1.27-alpine
COPY --from=build /app/dist /usr/share/nginx/html
EXPOSE 8080
`,
  },
  {
    key: 'rust-axum',
    label: 'Rust (Axum)',
    relDir: 'rust/axum-basic',
    port: 3000,
    healthPath: '/health',
    weight: 'heavy',
    dockerfile: `FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
# Cargo.toml package name is "axum-basic" -> binary target/release/axum-basic.
COPY --from=build /src/target/release/axum-basic /usr/local/bin/server
ENV PORT=3000
EXPOSE 3000
CMD ["/usr/local/bin/server"]
`,
  },
]

export function findExample(key: string): ExampleProject | undefined {
  return EXAMPLES.find((e) => e.key === key)
}

/** The fast, reliable default subset (light + medium, skips heavy builds). */
export const DEFAULT_SUBSET = ['go-gin', 'python-flask', 'node-nestjs']

/**
 * Render the example into a scratch build context (copying its source and the
 * generated Dockerfile) and `docker build` it. Returns the image ref to deploy.
 * Never mutates the repo source tree.
 *
 * IMPORTANT: Temps deploys an image by having the server `docker pull` it, so a
 * bare local tag (`temps-e2e-example/<key>`) is NOT deployable — the pull 404s
 * and the deploy fails (the proxy then serves the console fallback, masking the
 * failure). Pass `registry` (e.g. `localhost:5111`) to tag + push the image so
 * the server can actually pull it; the returned ref is the pushed registry ref.
 */
export async function buildExampleImage(
  ex: ExampleProject,
  opts: {
    examplesRoot: string
    scratchRoot: string
    /** Registry host[:port] to tag + push to (e.g. "localhost:5111"). */
    registry?: string
    tag?: string
    onLog?: (line: string) => void
  },
): Promise<string> {
  const srcDir = join(opts.examplesRoot, ex.relDir)
  const ctxDir = join(opts.scratchRoot, ex.key)
  const imageRef = opts.tag
    ? opts.tag
    : opts.registry
      ? `${opts.registry}/e2e-${ex.key}:latest`
      : `temps-e2e-example/${ex.key}:latest`

  await rm(ctxDir, { recursive: true, force: true })
  await mkdir(ctxDir, { recursive: true })
  // Copy the example source, skipping heavy/regenerable dirs so the build
  // context stays small and reproducible.
  await cp(srcDir, ctxDir, {
    recursive: true,
    filter: (s) =>
      !/(^|\/)(node_modules|target|dist|\.next|\.git)(\/|$)/.test(s),
  })
  await writeFile(join(ctxDir, 'Dockerfile'), ex.dockerfile, 'utf8')

  opts.onLog?.(`docker build -t ${imageRef} ${ctxDir}`)
  await dockerBuild(ctxDir, imageRef, opts.onLog)

  if (opts.registry) {
    opts.onLog?.(`docker push ${imageRef}`)
    await dockerPush(imageRef, opts.onLog)
  }
  return imageRef
}

/** Run `docker build`, streaming output through onLog; throws on non-zero exit. */
async function dockerBuild(
  contextDir: string,
  tag: string,
  onLog?: (line: string) => void,
): Promise<void> {
  await runDocker(['build', '--load', '-t', tag, contextDir], onLog, `docker build ${tag}`)
}

/** Run `docker push`, streaming output through onLog; throws on non-zero exit. */
async function dockerPush(tag: string, onLog?: (line: string) => void): Promise<void> {
  await runDocker(['push', tag], onLog, `docker push ${tag}`)
}

/** Spawn a docker subcommand, stream its output through onLog, throw on failure. */
async function runDocker(
  args: string[],
  onLog: ((line: string) => void) | undefined,
  what: string,
): Promise<void> {
  const proc = Bun.spawn(['docker', ...args], { stdout: 'pipe', stderr: 'pipe' })
  const pump = async (stream: ReadableStream<Uint8Array>) => {
    const reader = stream.getReader()
    const decoder = new TextDecoder()
    let buf = ''
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      const lines = buf.split('\n')
      buf = lines.pop() ?? ''
      for (const l of lines) if (l.trim()) onLog?.(l)
    }
    if (buf.trim()) onLog?.(buf)
  }
  await Promise.all([pump(proc.stdout), pump(proc.stderr)])
  const code = await proc.exited
  if (code !== 0) {
    throw new Error(`${what} failed (exit ${code})`)
  }
}
