/**
 * A throwaway Go app that serves a real HTML page at `/` -- built for the
 * analytics scenario, which needs the PROXY's own visitor/session cookie
 * issuance to actually fire.
 *
 * `temps-proxy::LoadBalancer::should_track_page` (proxy.rs) only calls
 * `ensure_visitor_session`/`set_tracking_cookies` for responses whose
 * content-type is `text/html` -- a plain-text "ok" response, like the other
 * throwaway apps in this suite return, never triggers cookie issuance at all.
 * This app exists solely to be a real tracked "website" so the scenario can
 * capture genuine `_temps_visitor_id`/`_temps_sid` Set-Cookie values from a
 * live GET, then replay them on `POST /api/_temps/event` the way a real
 * tracking snippet would.
 *
 * An HTML response is necessary but no longer sufficient: `should_track_page`
 * also requires the REQUEST to look like a browser top-level navigation
 * (`is_browser_document_request`: GET + an HTML `Accept` + `Sec-Fetch-Dest:
 * document`), so callers must send `BROWSER_DOCUMENT_HEADERS` from
 * `lib/flows.ts`. A bare `fetch()` gets no cookies, by design.
 *
 * No external Go dependencies (stdlib only) -- see toggle-app.ts's doc
 * comment for why that matters.
 */
import { mkdir, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const GO_MOD = `module analyticsapp

go 1.24
`

const MAIN_GO = `package main

import (
	"fmt"
	"net/http"
)

const homeHTML = ` + '`' + `<!doctype html>
<html>
<head><title>Analytics E2E App</title></head>
<body><h1>Analytics E2E App</h1><p>tracked page</p></body>
</html>
` + '`' + `

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(homeHTML))
	})
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	})

	fmt.Println("analytics-app listening on :3000")
	_ = http.ListenAndServe("0.0.0.0:3000", mux)
}
`

const DOCKERFILE = `FROM golang:1.24-alpine AS build
WORKDIR /src
COPY go.mod main.go ./
RUN CGO_ENABLED=0 GOOS=linux go build -o /server main.go

FROM gcr.io/distroless/static-debian12
COPY --from=build /server /server
ENV PORT=3000
EXPOSE 3000
ENTRYPOINT ["/server"]
`

export const ANALYTICS_APP_PORT = 3000

/** Render the analytics-app into a scratch build context and `docker build` (+ push) it. */
export async function buildAnalyticsAppImage(opts: {
  scratchRoot: string
  registry: string
  tag?: string
  onLog?: (line: string) => void
}): Promise<string> {
  const ctxDir = join(opts.scratchRoot, 'analytics-app')
  const imageRef = opts.tag ?? `${opts.registry}/e2e-analytics-app:latest`

  await rm(ctxDir, { recursive: true, force: true })
  await mkdir(ctxDir, { recursive: true })
  await writeFile(join(ctxDir, 'go.mod'), GO_MOD, 'utf8')
  await writeFile(join(ctxDir, 'main.go'), MAIN_GO, 'utf8')
  await writeFile(join(ctxDir, 'Dockerfile'), DOCKERFILE, 'utf8')

  opts.onLog?.(`docker build -t ${imageRef} ${ctxDir}`)
  await runDocker(['build', '--load', '-t', imageRef, ctxDir], opts.onLog, `docker build ${imageRef}`)
  opts.onLog?.(`docker push ${imageRef}`)
  await runDocker(['push', imageRef], opts.onLog, `docker push ${imageRef}`)
  return imageRef
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

/** Extract a cookie's raw value from a Set-Cookie header list ("name=value" segment only, no attributes). */
export function extractSetCookieValue(setCookieHeaders: string[], name: string): string | undefined {
  const prefix = `${name}=`
  for (const header of setCookieHeaders) {
    const firstSegment = header.split(';')[0]?.trim()
    if (firstSegment?.startsWith(prefix)) {
      return firstSegment.slice(prefix.length)
    }
  }
  return undefined
}
