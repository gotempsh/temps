// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Anonymous country geolocation.
//
// Derives the 2-letter ISO country code from the request's client IP using a
// bundled GeoLite2-Country database. The IP is used transiently for the lookup
// and is NEVER stored or logged — only the resulting country code is persisted,
// preserving the anonymous-by-design contract.
//
// The DB is loaded once at startup. In production it is REQUIRED: without it
// every event silently stores a NULL country, which once went unnoticed for
// weeks. `initGeo({ required: true })` therefore throws, the process exits, and
// the deployment fails its health check instead of shipping. Outside
// production a missing DB degrades gracefully to `null` country. Deployments
// that deliberately run without geolocation set GEOLITE2_COUNTRY_DB=disabled.

import { open, type Reader, type CountryResponse } from "maxmind";

// Path to the GeoLite2-Country.mmdb inside the image (provisioned at build).
// An empty value means "unset", not a path.
const DB_PATH = process.env.GEOLITE2_COUNTRY_DB || "/app/data/GeoLite2-Country.mmdb";

// Explicit opt-out value for GEOLITE2_COUNTRY_DB.
export const GEO_DISABLED = "disabled";

// A stable public IP that every GeoLite2 Country/City release resolves. A file
// that opens but can't resolve it is truncated or the wrong database.
const PROBE_IP = "8.8.8.8";

let _reader: Reader<CountryResponse> | null = null;

// Open a GeoLite2 DB and prove it resolves countries. Throws otherwise.
export async function openCountryDb(path: string): Promise<Reader<CountryResponse>> {
  const reader = await open<CountryResponse>(path);
  let probe: string | undefined;
  try {
    probe = reader.get(PROBE_IP)?.country?.iso_code;
  } catch {
    probe = undefined;
  }
  if (!probe) {
    throw new Error(
      `opened but resolved no country for probe IP ${PROBE_IP}; not a usable GeoLite2 Country/City database`
    );
  }
  return reader;
}

export async function initGeo(opts: { required: boolean; path?: string }): Promise<void> {
  const path = opts.path ?? DB_PATH;
  _reader = null;

  if (path === GEO_DISABLED) {
    console.warn("[geo] GEOLITE2_COUNTRY_DB=disabled; country geolocation disabled");
    return;
  }

  try {
    _reader = await openCountryDb(path);
    console.log(`[geo] loaded GeoLite2-Country from ${path}`);
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    if (opts.required) {
      throw new Error(
        `[geo] GeoLite2-Country DB not usable at ${path} (${reason}). Country ` +
          `geolocation is required in production: place GeoLite2-Country.mmdb in ` +
          `telemetry-api/data/ before building the image (or point ` +
          `GEOLITE2_COUNTRY_DB at a valid DB), or set ` +
          `GEOLITE2_COUNTRY_DB=${GEO_DISABLED} to run without it.`
      );
    }
    console.warn(
      `[geo] GeoLite2-Country DB not usable at ${path} (${reason}); country geolocation disabled`
    );
  }
}

// Extract the client IP from the proxy headers. The Temps proxy sets
// X-Forwarded-For with the real client IP; take the FIRST entry (the original
// client) and strip any port. Returns null if nothing usable.
export function clientIpFromHeaders(req: Request): string | null {
  const xff = req.headers.get("x-forwarded-for");
  if (xff) {
    const first = xff.split(",")[0]?.trim();
    if (first) return stripPort(first);
  }
  const real = req.headers.get("x-real-ip");
  if (real) return stripPort(real.trim());
  return null;
}

// IPv4 "1.2.3.4:5678" -> "1.2.3.4"; IPv6 is left as-is (bracketed forms rare in XFF).
function stripPort(ip: string): string {
  // Only strip a trailing :port for IPv4 (single colon). IPv6 has many colons.
  if (ip.includes(".") && ip.includes(":")) {
    return ip.split(":")[0] ?? ip;
  }
  return ip;
}

// Look up the 2-letter country code for an IP. Returns null when the DB is
// unavailable, the IP is unparseable/private, or no country is found. The IP is
// never retained beyond this call.
export function countryForIp(ip: string | null): string | null {
  if (!ip || !_reader) return null;
  try {
    const result = _reader.get(ip);
    return result?.country?.iso_code ?? null;
  } catch {
    // Invalid IP (e.g. private/loopback or malformed) — no country.
    return null;
  }
}

// Convenience: derive country directly from a request, never exposing the IP.
export function countryForRequest(req: Request): string | null {
  return countryForIp(clientIpFromHeaders(req));
}
