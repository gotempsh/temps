// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Mint an API key against a local dev instance through the product's own flow.
 *
 * `POST /api-keys` is a step-up-guarded sensitive action: it needs a browser
 * session with MFA enrolled and verified in the last five minutes. Rather than
 * reach around that guard, this enrols TOTP on the calling account, satisfies
 * the challenge for real, creates the key, and then unwinds the enrolment.
 *
 * Shared by every seeding/replay script so the unwind — the part that is easy
 * to get wrong and expensive to get wrong — exists in exactly one place.
 */

import { totp, waitForFreshWindow } from "./totp.ts";

/** The caller's authenticated JSON helper, already carrying a session cookie. */
export type ApiFn = <T>(path: string, init?: RequestInit) => Promise<T>;

interface MfaSetup {
  secret_key: string;
  recovery_codes?: string[];
}

export async function mintApiKey(api: ApiFn, name: string): Promise<string> {
  const setup = await api<MfaSetup>("/users/me/mfa/setup", { method: "POST" });
  const secret = setup.secret_key;

  try {
    await waitForFreshWindow();
    await api<void>("/users/me/mfa/verify", {
      method: "POST",
      body: JSON.stringify({ code: await totp(secret) }),
    });

    await api("/auth/step-up", {
      method: "POST",
      body: JSON.stringify({ code: await totp(secret) }),
    });

    const created = await api<{ api_key: string }>("/api-keys", {
      method: "POST",
      body: JSON.stringify({ name, role_type: "admin" }),
    });
    return created.api_key;
  } finally {
    // Always unwind, including when the steps above threw. MFA was turned on
    // purely to satisfy the step-up challenge; leaving it on with the TOTP
    // secret discarded would lock the developer out of their own dev instance
    // on the next login.
    await disableMfa(api, secret, setup.recovery_codes ?? []);
  }
}

/**
 * Turn MFA back off, and if that fails, print what the developer needs to get
 * back in rather than exiting with a silently-changed login.
 */
async function disableMfa(
  api: ApiFn,
  secret: string,
  recoveryCodes: string[],
): Promise<void> {
  try {
    await waitForFreshWindow();
    await api<void>("/users/me/mfa", {
      method: "DELETE",
      body: JSON.stringify({ code: await totp(secret) }),
    });
  } catch (err) {
    console.error(
      `\n!! Could not disable MFA (${err instanceof Error ? err.message : err}).\n` +
        `!! MFA is still ENABLED on this account. Keep these to sign in:\n` +
        `!!   TOTP secret:    ${secret}\n` +
        (recoveryCodes.length ? `!!   Recovery codes: ${recoveryCodes.join(", ")}\n` : "") +
        `!! Add the secret to an authenticator app, or disable MFA in the console.\n`,
    );
  }
}
