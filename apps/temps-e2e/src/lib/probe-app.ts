// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A throwaway Go app that proves a linked managed database is actually
 * usable, not just linked in the DB row: on boot it connects with whatever
 * `POSTGRES_URL` the deployment injects (the postgres provider's runtime env
 * var -- NOT `DATABASE_URL`, which is what mariadb/mongodb inject; see
 * crates/temps-providers/src/externalsvc/postgres.rs `build_runtime_env_vars`
 * / `get_runtime_env_vars`), creates a table, and on every
 * `/probe` request does a real INSERT then returns the real row count from a
 * real `SELECT count(*)`. There is no SQL-exec endpoint anywhere in the
 * platform API (`query_handlers.rs` is read-only row browsing, `services
 * connect` only prints connection info) -- deploying an app that writes via
 * the real injected `DATABASE_URL` is the only way to generate genuine
 * row-level data end to end.
 *
 * Deliberately NOT added to the repo's `examples/` tree: that's the
 * product's public example gallery (referenced from docs, `bootstrap.sh`),
 * not a place for test-only fixtures. This renders into a scratch build
 * context the same way `buildExampleImage` does, but from an inline source
 * instead of `examples/<relDir>`.
 */
import { mkdir, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const GO_MOD = `module probe

go 1.24

require github.com/lib/pq v1.10.9
`

const MAIN_GO = `package main

import (
	"database/sql"
	"encoding/json"
	"log"
	"net/http"
	"net/url"
	"os"
	"strings"

	_ "github.com/lib/pq"
)

var db *sql.DB

func main() {
	dsn := os.Getenv("POSTGRES_URL")
	if dsn == "" {
		log.Fatal("POSTGRES_URL not set")
	}
	// DNS-focused scenarios can preserve every injected credential/database
	// field while replacing only the address with a published *.temps.local
	// service-member record.
	if dnsAddress := os.Getenv("POSTGRES_DNS_ADDRESS"); dnsAddress != "" {
		parsed, err := url.Parse(dsn)
		if err != nil {
			log.Fatalf("parse POSTGRES_URL: %v", err)
		}
		parsed.Host = dnsAddress
		dsn = parsed.String()
	}
	// The injected DSN carries no sslmode param, and lib/pq defaults to
	// requiring SSL -- the managed container is plain internal Docker
	// network traffic with no TLS listener, so every connection attempt
	// fails with "SSL is not enabled on the server" without this.
	if !strings.Contains(dsn, "sslmode=") {
		sep := "?"
		if strings.Contains(dsn, "?") {
			sep = "&"
		}
		dsn += sep + "sslmode=disable"
	}
	var err error
	db, err = sql.Open("postgres", dsn)
	if err != nil {
		log.Fatalf("open: %v", err)
	}
	if _, err := db.Exec("CREATE TABLE IF NOT EXISTS e2e_probe (id serial primary key, created_at timestamptz default now())"); err != nil {
		log.Fatalf("create table: %v", err)
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("db-probe"))
	})
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		if err := db.Ping(); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	})
	mux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
		if _, err := db.Exec("INSERT INTO e2e_probe DEFAULT VALUES"); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		var count int
		if err := db.QueryRow("SELECT count(*) FROM e2e_probe").Scan(&count); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]int{"count": count})
	})
	mux.HandleFunc("/database-info", func(w http.ResponseWriter, r *http.Request) {
		parsedDSN, err := url.Parse(dsn)
		if err != nil {
			http.Error(w, "invalid POSTGRES_URL", http.StatusInternalServerError)
			return
		}
		if err := db.PingContext(r.Context()); err != nil {
			http.Error(w, "database ping failed", http.StatusBadGateway)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]string{
			"database_host": parsedDSN.Host,
		})
	})

	port := os.Getenv("PORT")
	if port == "" {
		port = "3000"
	}
	log.Printf("probe listening on :%s", port)
	log.Fatal(http.ListenAndServe("0.0.0.0:"+port, mux))
}
`

// go.sum is intentionally NOT hand-written (its hashes would need to be
// reproduced exactly) -- `go mod tidy` in the build step computes it fresh
// against the one pinned dependency, same network requirement the repo's
// other Go example already has via `go mod download`.
const DOCKERFILE = `FROM golang:1.24-alpine AS build
WORKDIR /src
COPY go.mod main.go ./
RUN go mod tidy && CGO_ENABLED=0 GOOS=linux go build -o /server main.go

FROM gcr.io/distroless/static-debian12
COPY --from=build /server /server
ENV PORT=3000
EXPOSE 3000
ENTRYPOINT ["/server"]
`

export const PROBE_APP_PORT = 3000
export const PROBE_APP_HEALTH_PATH = '/health'

/** Render the probe app into a scratch build context and `docker build` (+ optional push) it. */
export async function buildProbeImage(opts: {
  scratchRoot: string
  registry: string
  tag?: string
  onLog?: (line: string) => void
}): Promise<string> {
  const ctxDir = join(opts.scratchRoot, 'db-probe')
  const imageRef = opts.tag ?? `${opts.registry}/e2e-db-probe:latest`

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

/**
 * pgx-based variant of the probe app used by the HA failover and multinode
 * scenarios. It is NOT a drop-in replacement for `buildProbeImage` above,
 * which standalone-service scenarios keep using against single-host,
 * single-port services where `lib/pq` works fine.
 *
 * Multi-host HA specifically needs a driver that actually understands
 * `postgresql://host1:port1,host2:port2/db?target_session_attrs=read-write`.
 * `lib/pq` -- the import in `MAIN_GO` above -- cannot: verified live
 * (`go run` against this exact cluster's real hosts/ports) that its latest
 * *released* version (v1.10.9, what `go get github.com/lib/pq` actually
 * resolves to) has no multi-host or `target_session_attrs` support at all --
 * that code exists only on lib/pq's unreleased `master` branch, and the
 * project has been in maintenance mode since 2022, pointing new users at
 * `github.com/jackc/pgx` instead. `pgx`'s `stdlib` driver parses the exact
 * same connection string the platform emits and correctly routes writes to
 * whichever host currently satisfies `target_session_attrs=read-write` --
 * also verified live, including across a real `docker stop` of the primary.
 */
const MAIN_GO_PGX = MAIN_GO.replace(
  '\t_ "github.com/lib/pq"\n',
  '\t_ "github.com/jackc/pgx/v5/stdlib"\n',
).replace('sql.Open("postgres", dsn)', 'sql.Open("pgx", dsn)')

const GO_MOD_PGX = `module probe

go 1.24

require github.com/jackc/pgx/v5 v5.7.1
`

export async function buildHaProbeImage(opts: {
  scratchRoot: string
  registry: string
  tag?: string
  push?: boolean
  onLog?: (line: string) => void
}): Promise<string> {
  const ctxDir = join(opts.scratchRoot, 'db-probe-ha')
  const imageRef = opts.tag ?? `${opts.registry}/e2e-db-probe-ha:latest`

  await rm(ctxDir, { recursive: true, force: true })
  await mkdir(ctxDir, { recursive: true })
  await writeFile(join(ctxDir, 'go.mod'), GO_MOD_PGX, 'utf8')
  await writeFile(join(ctxDir, 'main.go'), MAIN_GO_PGX, 'utf8')
  await writeFile(join(ctxDir, 'Dockerfile'), DOCKERFILE, 'utf8')

  // This HA probe can be transferred with `docker save` into a nested Docker
  // daemon. BuildKit's default provenance attestation wraps a single-platform
  // image in an image index when the runner uses the containerd image store.
  // That index is not preserved portably by the save/load hand-off and newer
  // daemons reject it during signature-chain validation. The probe does not
  // publish supply-chain artifacts, so emit a plain single-platform image.
  opts.onLog?.(`docker build --provenance=false -t ${imageRef} ${ctxDir}`)
  await runDocker(
    ['build', '--load', '--provenance=false', '-t', imageRef, ctxDir],
    opts.onLog,
    `docker build ${imageRef}`,
  )
  if (opts.push !== false) {
    opts.onLog?.(`docker push ${imageRef}`)
    await runDocker(['push', imageRef], opts.onLog, `docker push ${imageRef}`)
  }
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
