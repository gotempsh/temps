/**
 * RFC 6238 TOTP generation, so seeding scripts can drive the real MFA and
 * step-up flows instead of reaching around them.
 *
 * Matches the server's defaults: SHA-1, 6 digits, 30-second period.
 */

const BASE32_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

function base32Decode(input: string): Uint8Array<ArrayBuffer> {
  const clean = input.replace(/=+$/, "").replace(/\s+/g, "").toUpperCase();
  let bits = 0;
  let value = 0;
  const out: number[] = [];
  for (const char of clean) {
    const idx = BASE32_ALPHABET.indexOf(char);
    if (idx === -1) throw new Error(`invalid base32 character '${char}' in TOTP secret`);
    value = (value << 5) | idx;
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      out.push((value >>> bits) & 0xff);
    }
  }
  return new Uint8Array(out);
}

/** Current TOTP code for a base32 secret. */
export async function totp(secretBase32: string, atMs: number = Date.now()): Promise<string> {
  const counter = BigInt(Math.floor(atMs / 1000 / 30));
  const counterBytes = new Uint8Array(8);
  new DataView(counterBytes.buffer).setBigUint64(0, counter, false);

  const key = await crypto.subtle.importKey(
    "raw",
    base32Decode(secretBase32),
    { name: "HMAC", hash: "SHA-1" },
    false,
    ["sign"],
  );
  const mac = new Uint8Array(await crypto.subtle.sign("HMAC", key, counterBytes));

  // Dynamic truncation (RFC 4226 §5.4).
  const offset = mac[mac.length - 1]! & 0x0f;
  const binary =
    ((mac[offset]! & 0x7f) << 24) |
    ((mac[offset + 1]! & 0xff) << 16) |
    ((mac[offset + 2]! & 0xff) << 8) |
    (mac[offset + 3]! & 0xff);
  return String(binary % 1_000_000).padStart(6, "0");
}

/** Wait until the current 30s TOTP window has at least `seconds` left. */
export async function waitForFreshWindow(seconds = 3): Promise<void> {
  const remaining = 30 - (Math.floor(Date.now() / 1000) % 30);
  if (remaining < seconds) {
    await new Promise((resolve) => setTimeout(resolve, remaining * 1000 + 500));
  }
}
