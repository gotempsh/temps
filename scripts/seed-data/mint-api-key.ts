/**
 * Mint an API key against a local dev instance and print it.
 *
 * `POST /api-keys` is a step-up-guarded sensitive action, so this drives the
 * real flow — enrol TOTP, verify, step up, create the key, then disable MFA
 * again — rather than reaching around the guard. Handy for pointing
 * `TEMPS_API_KEY` at a dev server when testing the CLI.
 *
 * Usage:
 *   TEMPS_API=http://localhost:8261 TEMPS_PASSWORD=... bun run mint-api-key.ts
 */

import { mintApiKey } from "./admin-key.ts";

const API = process.env.TEMPS_API || "http://localhost:8080";
const EMAIL = process.env.TEMPS_EMAIL || "dev@temps.sh";
const PASSWORD = process.env.TEMPS_PASSWORD;

if (!PASSWORD) {
  console.error("TEMPS_PASSWORD is required");
  process.exit(1);
}

let sessionCookie = "";

async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(`${API}/api${path}`, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(sessionCookie ? { Cookie: sessionCookie } : {}),
      ...(init.headers || {}),
    },
  });
  const text = await res.text();
  if (!res.ok) {
    throw new Error(`${init.method || "GET"} ${path} -> ${res.status}: ${text.slice(0, 400)}`);
  }
  return (text ? JSON.parse(text) : undefined) as T;
}

const login = await fetch(`${API}/api/auth/login`, {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify({ email: EMAIL, password: PASSWORD }),
});
if (!login.ok) throw new Error(`login -> ${login.status}: ${await login.text()}`);
const session = (login.headers.getSetCookie?.() ?? [])
  .map((c) => c.split(";")[0]!)
  .find((c) => c.startsWith("session=") && c !== "session=");
if (!session) throw new Error("login returned no session cookie");
sessionCookie = session;

const apiKey = await mintApiKey(api, `cli-dev-${Date.now()}`);

console.log(apiKey);
