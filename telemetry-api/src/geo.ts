// Anonymous country geolocation.
//
// Derives the 2-letter ISO country code from the request's client IP using a
// bundled GeoLite2-Country database. The IP is used transiently for the lookup
// and is NEVER stored or logged — only the resulting country code is persisted,
// preserving the anonymous-by-design contract.
//
// The DB is loaded once at startup. If it's missing (e.g. not provisioned in a
// dev environment), geolocation degrades gracefully to `null` country.

import { open, type Reader, type CountryResponse } from "maxmind";

// Path to the GeoLite2-Country.mmdb inside the image (provisioned at build).
const DB_PATH = process.env.GEOLITE2_COUNTRY_DB ?? "/app/data/GeoLite2-Country.mmdb";

let _reader: Reader<CountryResponse> | null = null;
let _loadAttempted = false;

export async function initGeo(): Promise<void> {
  if (_loadAttempted) return;
  _loadAttempted = true;
  try {
    _reader = await open<CountryResponse>(DB_PATH);
    console.log(`[geo] loaded GeoLite2-Country from ${DB_PATH}`);
  } catch (err) {
    _reader = null;
    console.warn(
      `[geo] GeoLite2-Country DB not available at ${DB_PATH} (${
        err instanceof Error ? err.message : String(err)
      }); country geolocation disabled`,
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
