import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CodeBlock } from '@/components/ui/code-block'
import { CopyButton } from '@/components/ui/copy-button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Info } from 'lucide-react'

interface FlagsSetupGuideProps {
  /** Environment the flags table is currently showing, so the snippets ask for the same one. */
  environmentId?: number
  environmentName?: string
  /** An existing flag key makes the examples copy-paste runnable; falls back to a placeholder. */
  sampleFlagKey?: string
}

/**
 * Integration guide for reading flags from a running application.
 *
 * There is no per-language Temps flag SDK, and inventing one for six ecosystems
 * is not the same job as shipping flags. What every language *does* have is an
 * HTTP client, so each snippet is a complete, self-contained client against the
 * two public endpoints:
 *
 *   GET  $TEMPS_API_URL/flags/snapshot   -> every flag for the token's environment
 *   POST $TEMPS_API_URL/flags/exposure   -> which keys the app actually read
 *
 * Every snippet implements the same four rules, because getting any of them
 * wrong is what turns a flag into an outage:
 *   1. Poll in the background and evaluate from memory — never block a request
 *      on the control plane.
 *   2. Send If-None-Match, so the steady state is a 304.
 *   3. On any failure (network, 5xx, malformed value) serve the caller's own
 *      fallback rather than throwing.
 *   4. `enabled: false` is the kill switch — serve `default_value`, ignoring
 *      any environment override.
 */
export function FlagsSetupGuide({
  environmentId,
  environmentName,
  sampleFlagKey,
}: FlagsSetupGuideProps) {
  const flagKey = sampleFlagKey ?? 'checkout.v2'
  // A deployment token is normally pinned to one environment and the query
  // parameter is ignored. Project-wide tokens must say which environment they
  // mean, so the snippets carry it — using the one the operator is looking at.
  const envQuery = environmentId ? `?environment_id=${environmentId}` : ''
  const envSuffix = environmentName ? ` (${environmentName})` : ''

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle className="text-base">1. Credentials</CardTitle>
          <CardDescription>
            Apps deployed on this instance already have these. Set them yourself
            only when running locally or somewhere else — a deployment token
            pins the project and environment, so no app can read another
            project&apos;s flags.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <CodeBlock
            code={`# Injected automatically into every deployment on Temps
TEMPS_API_URL=https://your-instance.temps.dev/api
TEMPS_API_TOKEN=dt_...`}
            language="bash"
          />
          <p className="text-sm text-muted-foreground">
            Verify the token can see this environment{envSuffix} before wiring
            up any code:
          </p>
          <CodeBlock
            code={`curl -s -H "Authorization: Bearer $TEMPS_API_TOKEN" \\
  "$TEMPS_API_URL/flags/snapshot${envQuery}"`}
            language="bash"
          />
          <Alert>
            <Info className="h-4 w-4" />
            <AlertDescription>
              A <strong>400 Deployment Token Required</strong> means you used a
              session or API key. Those read flags through{' '}
              <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
                /projects/{'{project_id}'}/flags
              </code>{' '}
              instead — the snapshot endpoint is only for running apps.
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div className="flex items-start justify-between gap-2">
            <div className="space-y-1.5">
              <CardTitle className="text-base">
                2. Read flags from your app
              </CardTitle>
              <CardDescription>
                Each snippet is a complete client: it polls in the background,
                caches with an ETag, evaluates in memory, and falls back to your
                own default whenever Temps is unreachable. No request of yours
                ever waits on the control plane.
              </CardDescription>
            </div>
            <CopyButton
              value={aiPrompt(flagKey, envQuery)}
              className="shrink-0 rounded-md border border-border px-3 py-1.5 text-xs font-medium"
            >
              Copy AI Prompt
            </CopyButton>
          </div>
        </CardHeader>
        <CardContent>
          <Tabs defaultValue="node" className="w-full">
            <TabsList className="flex h-auto w-full flex-wrap justify-start gap-1">
              <TabsTrigger value="node">Node.js</TabsTrigger>
              <TabsTrigger value="python">Python</TabsTrigger>
              <TabsTrigger value="go">Go</TabsTrigger>
              <TabsTrigger value="ruby">Ruby</TabsTrigger>
              <TabsTrigger value="php">PHP</TabsTrigger>
              <TabsTrigger value="java">Java</TabsTrigger>
              <TabsTrigger value="curl">Any language</TabsTrigger>
            </TabsList>

            <TabsContent value="node" className="mt-4">
              <CodeBlock
                code={nodeSnippet(flagKey, envQuery)}
                language="typescript"
              />
            </TabsContent>
            <TabsContent value="python" className="mt-4">
              <CodeBlock
                code={pythonSnippet(flagKey, envQuery)}
                language="python"
              />
            </TabsContent>
            <TabsContent value="go" className="mt-4">
              <CodeBlock code={goSnippet(flagKey, envQuery)} language="go" />
            </TabsContent>
            <TabsContent value="ruby" className="mt-4">
              <CodeBlock
                code={rubySnippet(flagKey, envQuery)}
                language="ruby"
              />
            </TabsContent>
            <TabsContent value="php" className="mt-4">
              <CodeBlock code={phpSnippet(flagKey, envQuery)} language="php" />
            </TabsContent>
            <TabsContent value="java" className="mt-4">
              <CodeBlock
                code={javaSnippet(flagKey, envQuery)}
                language="java"
              />
            </TabsContent>
            <TabsContent value="curl" className="mt-4 space-y-3">
              <p className="text-sm text-muted-foreground">
                The whole contract is two endpoints and one evaluation rule, so
                a client in any language is about forty lines.
              </p>
              <CodeBlock code={wireContract(envQuery)} language="bash" />
            </TabsContent>
          </Tabs>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">
            3. Report which flags you read
          </CardTitle>
          <CardDescription>
            Optional, and worth it. Evaluation happens inside your process, so
            without this the control plane cannot tell a flag live code depends
            on from one nothing has called in a year — which is what makes flags
            safe to delete. Batch the keys and post them on an interval; the
            endpoint only stamps a timestamp and can never change what a flag
            serves.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <CodeBlock
            code={`curl -s -X POST -H "Authorization: Bearer $TEMPS_API_TOKEN" \\
  -H "Content-Type: application/json" \\
  -d '{"keys": ["${flagKey}"]}' \\
  "$TEMPS_API_URL/flags/exposure"`}
            language="bash"
          />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">
            4. Manage flags from anywhere
          </CardTitle>
          <CardDescription>
            Everything on this page is also a CLI command, so flag flips can
            live in scripts, CI, and incident runbooks instead of only in a
            browser tab.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <CodeBlock
            code={cliSnippet(flagKey, environmentName)}
            language="bash"
          />
        </CardContent>
      </Card>
    </div>
  )
}

// =============================================================================
// Snippets
//
// Kept as plain functions rather than a template map: each ecosystem has a
// different idiom for background work and HTTP, and flattening them into one
// parameterised template produces code nobody would actually write.
// =============================================================================

function nodeSnippet(flagKey: string, envQuery: string): string {
  return `// flags.ts — no dependencies, works in Node 18+ and Bun.
const API_URL = process.env.TEMPS_API_URL!
const TOKEN = process.env.TEMPS_API_TOKEN!

type Flag = {
  key: string
  value_type: 'bool' | 'string' | 'number' | 'json'
  default_value: unknown
  enabled: boolean
  environment_value: unknown | null
}

let cache = new Map<string, Flag>()
let etag: string | null = null

async function refresh() {
  const res = await fetch(\`\${API_URL}/flags/snapshot${envQuery}\`, {
    headers: {
      Authorization: \`Bearer \${TOKEN}\`,
      ...(etag ? { 'If-None-Match': etag } : {}),
    },
  })
  if (res.status === 304) return // unchanged — the common case
  if (!res.ok) throw new Error(\`flag snapshot: \${res.status}\`)
  etag = res.headers.get('etag')
  const body = (await res.json()) as { flags: Flag[] }
  cache = new Map(body.flags.map((f) => [f.key, f]))
}

/** Never throws, never blocks: an unreachable control plane serves your fallback. */
export function flag<T>(key: string, fallback: T): T {
  const f = cache.get(key)
  if (!f) return fallback
  // enabled: false is the kill switch — the override is ignored.
  const value = f.enabled ? (f.environment_value ?? f.default_value) : f.default_value
  return (value as T) ?? fallback
}

/** Await once at boot so the first request already has real values. */
export async function startFlags(intervalMs = 15_000) {
  await refresh().catch((e) => console.warn('flags: initial fetch failed', e))
  setInterval(() => refresh().catch(() => {}), intervalMs).unref()
}

// Usage
// await startFlags()
// if (flag('${flagKey}', false)) { /* new path */ }`
}

function pythonSnippet(flagKey: string, envQuery: string): string {
  return `# flags.py — requires: pip install requests
import os, threading, time, requests

API_URL = os.environ["TEMPS_API_URL"]
TOKEN = os.environ["TEMPS_API_TOKEN"]

_cache: dict[str, dict] = {}
_etag: str | None = None
_lock = threading.Lock()


def _refresh() -> None:
    global _etag
    headers = {"Authorization": f"Bearer {TOKEN}"}
    if _etag:
        headers["If-None-Match"] = _etag
    res = requests.get(f"{API_URL}/flags/snapshot${envQuery}", headers=headers, timeout=5)
    if res.status_code == 304:  # unchanged — the common case
        return
    res.raise_for_status()
    _etag = res.headers.get("ETag")
    with _lock:
        _cache.clear()
        _cache.update({f["key"]: f for f in res.json()["flags"]})


def flag(key: str, fallback):
    """Never raises, never blocks: an unreachable control plane serves your fallback."""
    with _lock:
        f = _cache.get(key)
    if f is None:
        return fallback
    # enabled: False is the kill switch — the override is ignored.
    value = (f["environment_value"] if f["enabled"] else None)
    if value is None:
        value = f["default_value"]
    return fallback if value is None else value


def start_flags(interval: float = 15.0) -> None:
    """Call once at startup. Blocks only for the first fetch."""
    try:
        _refresh()
    except Exception as e:  # noqa: BLE001 — flags must never break boot
        print(f"flags: initial fetch failed: {e}")

    def loop() -> None:
        while True:
            time.sleep(interval)
            try:
                _refresh()
            except Exception:  # noqa: BLE001
                pass

    threading.Thread(target=loop, daemon=True).start()


# Usage
# start_flags()
# if flag("${flagKey}", False):
#     ...`
}

function goSnippet(flagKey: string, envQuery: string): string {
  return `// flags.go — standard library only.
package flags

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"sync"
	"time"
)

type Flag struct {
	Key              string          \`json:"key"\`
	ValueType        string          \`json:"value_type"\`
	DefaultValue     json.RawMessage \`json:"default_value"\`
	Enabled          bool            \`json:"enabled"\`
	EnvironmentValue json.RawMessage \`json:"environment_value"\`
}

var (
	mu    sync.RWMutex
	cache = map[string]Flag{}
	etag  string
	http5 = &http.Client{Timeout: 5 * time.Second}
)

func refresh() error {
	req, err := http.NewRequest("GET", os.Getenv("TEMPS_API_URL")+"/flags/snapshot${envQuery}", nil)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+os.Getenv("TEMPS_API_TOKEN"))
	mu.RLock()
	if etag != "" {
		req.Header.Set("If-None-Match", etag)
	}
	mu.RUnlock()

	res, err := http5.Do(req)
	if err != nil {
		return err
	}
	defer res.Body.Close()
	if res.StatusCode == http.StatusNotModified { // unchanged — the common case
		return nil
	}
	if res.StatusCode != http.StatusOK {
		return fmt.Errorf("flag snapshot: %s", res.Status)
	}
	var body struct {
		Flags []Flag \`json:"flags"\`
	}
	if err := json.NewDecoder(res.Body).Decode(&body); err != nil {
		return err
	}
	next := make(map[string]Flag, len(body.Flags))
	for _, f := range body.Flags {
		next[f.Key] = f
	}
	mu.Lock()
	cache, etag = next, res.Header.Get("ETag")
	mu.Unlock()
	return nil
}

// Bool never errors and never blocks: an unreachable control plane serves your fallback.
func Bool(key string, fallback bool) bool {
	mu.RLock()
	f, ok := cache[key]
	mu.RUnlock()
	if !ok {
		return fallback
	}
	raw := f.DefaultValue
	// Enabled == false is the kill switch — the override is ignored.
	if f.Enabled && len(f.EnvironmentValue) > 0 && string(f.EnvironmentValue) != "null" {
		raw = f.EnvironmentValue
	}
	var v bool
	if err := json.Unmarshal(raw, &v); err != nil {
		return fallback
	}
	return v
}

// Start once at boot. The first fetch is synchronous so the first request has real values.
func Start(interval time.Duration) {
	if err := refresh(); err != nil {
		fmt.Fprintf(os.Stderr, "flags: initial fetch failed: %v\\n", err)
	}
	go func() {
		for range time.Tick(interval) {
			_ = refresh()
		}
	}()
}

// Usage
// flags.Start(15 * time.Second)
// if flags.Bool("${flagKey}", false) { ... }`
}

function rubySnippet(flagKey: string, envQuery: string): string {
  return `# flags.rb — standard library only.
require 'json'
require 'net/http'

module Flags
  API_URL = ENV.fetch('TEMPS_API_URL')
  TOKEN = ENV.fetch('TEMPS_API_TOKEN')

  @cache = {}
  @etag = nil
  @mutex = Mutex.new

  class << self
    def refresh
      uri = URI("#{API_URL}/flags/snapshot${envQuery}")
      req = Net::HTTP::Get.new(uri)
      req['Authorization'] = "Bearer #{TOKEN}"
      req['If-None-Match'] = @etag if @etag

      res = Net::HTTP.start(uri.hostname, uri.port, use_ssl: uri.scheme == 'https',
                            open_timeout: 5, read_timeout: 5) { |h| h.request(req) }
      return if res.code == '304' # unchanged — the common case
      raise "flag snapshot: #{res.code}" unless res.code == '200'

      flags = JSON.parse(res.body)['flags'].each_with_object({}) { |f, h| h[f['key']] = f }
      @mutex.synchronize { @cache = flags; @etag = res['ETag'] }
    end

    # Never raises, never blocks: an unreachable control plane serves your fallback.
    def get(key, fallback)
      f = @mutex.synchronize { @cache[key] }
      return fallback if f.nil?

      # enabled: false is the kill switch — the override is ignored.
      value = f['enabled'] ? f['environment_value'] : nil
      value = f['default_value'] if value.nil?
      value.nil? ? fallback : value
    end

    def start(interval = 15)
      begin
        refresh
      rescue StandardError => e
        warn "flags: initial fetch failed: #{e}"
      end
      Thread.new do
        loop do
          sleep interval
          begin
            refresh
          rescue StandardError
            nil
          end
        end
      end
    end
  end
end

# Usage
# Flags.start
# if Flags.get('${flagKey}', false)`
}

function phpSnippet(flagKey: string, envQuery: string): string {
  return `<?php
// Flags.php — no dependencies. PHP is request-scoped, so cache the snapshot on
// disk (or APCu/Redis) instead of in a background thread: one process refreshes
// it, every request reads it.

final class Flags
{
    private const TTL = 15; // seconds
    private const CACHE = '/tmp/temps-flags.json';

    private static array $flags = [];

    public static function load(): void
    {
        $cached = @file_get_contents(self::CACHE);
        $fresh = $cached !== false && (time() - @filemtime(self::CACHE)) < self::TTL;

        if ($fresh) {
            self::$flags = json_decode($cached, true)['flags'] ?? [];
            return;
        }

        $ch = curl_init(getenv('TEMPS_API_URL') . '/flags/snapshot${envQuery}');
        curl_setopt_array($ch, [
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_TIMEOUT => 5,
            CURLOPT_HTTPHEADER => ['Authorization: Bearer ' . getenv('TEMPS_API_TOKEN')],
        ]);
        $body = curl_exec($ch);
        $status = curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        curl_close($ch);

        if ($status === 200 && $body !== false) {
            @file_put_contents(self::CACHE, $body, LOCK_EX);
            self::$flags = json_decode($body, true)['flags'] ?? [];
        } elseif ($cached !== false) {
            // Control plane unreachable: keep serving the last good snapshot
            // rather than losing every flag at once.
            self::$flags = json_decode($cached, true)['flags'] ?? [];
        }
    }

    /** Never throws: an unreachable control plane serves your fallback. */
    public static function get(string $key, mixed $fallback): mixed
    {
        foreach (self::$flags as $f) {
            if ($f['key'] !== $key) {
                continue;
            }
            // enabled: false is the kill switch — the override is ignored.
            $value = $f['enabled'] ? ($f['environment_value'] ?? null) : null;
            return $value ?? $f['default_value'] ?? $fallback;
        }
        return $fallback;
    }
}

// Usage (bootstrap)
// Flags::load();
// if (Flags::get('${flagKey}', false)) { ... }`
}

function javaSnippet(flagKey: string, envQuery: string): string {
  return `// Flags.java — JDK 11+, no dependencies beyond a JSON parser (Jackson here).
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.net.URI;
import java.net.http.*;
import java.time.Duration;
import java.util.*;
import java.util.concurrent.*;

public final class Flags {
    private static final String API_URL = System.getenv("TEMPS_API_URL");
    private static final String TOKEN = System.getenv("TEMPS_API_TOKEN");
    private static final HttpClient HTTP =
            HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(5)).build();
    private static final ObjectMapper JSON = new ObjectMapper();

    private static volatile Map<String, JsonNode> cache = Map.of();
    private static volatile String etag = null;

    private static void refresh() throws Exception {
        var builder = HttpRequest.newBuilder(URI.create(API_URL + "/flags/snapshot${envQuery}"))
                .header("Authorization", "Bearer " + TOKEN)
                .timeout(Duration.ofSeconds(5));
        if (etag != null) builder.header("If-None-Match", etag);

        HttpResponse<String> res = HTTP.send(builder.build(), HttpResponse.BodyHandlers.ofString());
        if (res.statusCode() == 304) return; // unchanged — the common case
        if (res.statusCode() != 200) throw new IllegalStateException("flag snapshot: " + res.statusCode());

        Map<String, JsonNode> next = new HashMap<>();
        for (JsonNode f : JSON.readTree(res.body()).get("flags")) next.put(f.get("key").asText(), f);
        cache = Map.copyOf(next);
        etag = res.headers().firstValue("etag").orElse(null);
    }

    /** Never throws, never blocks: an unreachable control plane serves your fallback. */
    public static boolean bool(String key, boolean fallback) {
        JsonNode f = cache.get(key);
        if (f == null) return fallback;
        // enabled: false is the kill switch — the override is ignored.
        JsonNode value = f.get("default_value");
        JsonNode override = f.get("environment_value");
        if (f.get("enabled").asBoolean() && override != null && !override.isNull()) value = override;
        return value != null && value.isBoolean() ? value.asBoolean() : fallback;
    }

    /** Call once at startup. The first fetch is synchronous. */
    public static void start(Duration interval) {
        try {
            refresh();
        } catch (Exception e) {
            System.err.println("flags: initial fetch failed: " + e.getMessage());
        }
        Executors.newSingleThreadScheduledExecutor(r -> {
            Thread t = new Thread(r, "temps-flags");
            t.setDaemon(true);
            return t;
        }).scheduleAtFixedRate(() -> {
            try {
                refresh();
            } catch (Exception ignored) {
            }
        }, interval.toSeconds(), interval.toSeconds(), TimeUnit.SECONDS);
    }
}

// Usage
// Flags.start(Duration.ofSeconds(15));
// if (Flags.bool("${flagKey}", false)) { ... }`
}

function wireContract(envQuery: string): string {
  return `# 1. Fetch every flag for the token's environment.
#    Send If-None-Match to make the steady state a cheap 304.
curl -s -D- -H "Authorization: Bearer $TEMPS_API_TOKEN" \\
  "$TEMPS_API_URL/flags/snapshot${envQuery}"

# ETag: "9f2c..."
# {
#   "environment_id": 3,
#   "flags": [
#     {
#       "key": "checkout.v2",
#       "value_type": "bool",         # bool | string | number | json
#       "default_value": false,       # served when nothing more specific applies
#       "enabled": true,              # false = kill switch for this environment
#       "environment_value": true     # null = inherit default_value
#     }
#   ]
# }

# 2. Evaluate locally, in this exact order:
#    a. key missing from the snapshot -> your own fallback
#    b. enabled == false              -> default_value  (ignore the override)
#    c. environment_value != null     -> environment_value
#    d. otherwise                     -> default_value
#    e. value does not match value_type, or any error -> your own fallback
#
# 3. Refresh every ~15s in the background. Never fetch on the request path:
#    a flag lookup must not add a network hop to your p99, and your app must
#    keep serving the last good snapshot when the control plane is down.

# 4. Optional: report the keys you read so unused flags become visible.
curl -s -X POST -H "Authorization: Bearer $TEMPS_API_TOKEN" \\
  -H "Content-Type: application/json" \\
  -d '{"keys": ["checkout.v2"]}' \\
  "$TEMPS_API_URL/flags/exposure"`
}

function cliSnippet(flagKey: string, environmentName?: string): string {
  const env = environmentName ?? 'production'
  return `# Create a flag (type and default are fixed at create time)
bunx @temps-sdk/cli flags create ${flagKey} --type bool --default false \\
  --description "New checkout flow"

# Turn it on in one environment
bunx @temps-sdk/cli flags set ${flagKey} true --environment ${env}

# Kill switch: serve the default again, ignoring the override
bunx @temps-sdk/cli flags disable ${flagKey} --environment ${env}

# See where it stands everywhere
bunx @temps-sdk/cli flags get ${flagKey}

# Retire it once the code path is gone
bunx @temps-sdk/cli flags archive ${flagKey}`
}

function aiPrompt(flagKey: string, envQuery: string): string {
  return `Add Temps feature flags to this application.

Temps injects TEMPS_API_URL and TEMPS_API_TOKEN into deployments. Do not add a
new SDK dependency — implement a small client using the HTTP endpoints below,
in the language and idiom this codebase already uses.

Endpoints:
  GET  $TEMPS_API_URL/flags/snapshot${envQuery}
       Header: Authorization: Bearer $TEMPS_API_TOKEN
       Header: If-None-Match: <last ETag>   (returns 304 when unchanged)
       200 body: { "environment_id": 3, "flags": [
         { "key": "${flagKey}", "value_type": "bool" | "string" | "number" | "json",
           "default_value": <any>, "enabled": true, "environment_value": <any|null> } ] }

  POST $TEMPS_API_URL/flags/exposure
       Body: { "keys": ["${flagKey}"] }   (optional; marks flags as still in use)

Requirements:
1. Fetch the snapshot in a background loop (~15s) and cache it in memory. Never
   fetch on the request path.
2. Send If-None-Match and treat 304 as "keep the cached snapshot".
3. Evaluate locally in this order: key missing -> caller's fallback;
   enabled == false -> default_value (the override is ignored); otherwise
   environment_value if non-null, else default_value.
4. The lookup function must never throw and never block. Any failure — network
   error, non-200, wrong type for value_type — serves the caller's fallback.
5. Do the first fetch synchronously at startup so the first request has real
   values, but a failed first fetch must not prevent the app from booting.
6. Add one call site as an example, using a caller-supplied fallback that
   matches today's behaviour.`
}
