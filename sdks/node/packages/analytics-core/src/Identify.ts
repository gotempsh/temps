// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `identify()` and `reset()` for the browser (ADR 109 D4).
 *
 * `identify` labels the visitor the request already resolves to: the
 * proxy's `_temps_visitor_id` cookie on a site Temps serves, or the
 * tracker's stored id on a keyed site. The browser never names any other
 * visitor. In cookieless mode nothing is sent at all: a user id on a daily
 * visitor would link its days together, which is what cookieless rules out.
 */

import type { IdentifyResult, JsonValue } from "./types";
import { ingestKeyHeaders } from "./utils";
import { clearStoredIdentity, getOrCreateVisitorId, isCookielessIdentity, notifyIdentityReset } from "./identity";

/** Remembers the last identify that was applied, so repeats are not resent. */
const IDENTIFIED_KEY = "temps_identified";

export const IDENTIFY_ENDPOINT = "identify";
export const RESET_ENDPOINT = "reset";

/** Server outcomes that mean the label is on the visitor now. */
const APPLIED_OUTCOMES = new Set(["updated", "unchanged"]);

export interface IdentifyVisitorOptions {
  basePath: string;
  ingestKey?: string;
  userId: string;
  traits?: Record<string, JsonValue>;
  /**
   * Resolves once the page view that creates the visitor has been sent. A
   * first visit's visitor only exists after its first event.
   */
  ready?: Promise<unknown>;
}

function memoFor(userId: string, traits: Record<string, JsonValue> | undefined): string {
  return JSON.stringify([userId, traits ?? null]);
}

function readMemo(): string | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage.getItem(IDENTIFIED_KEY);
  } catch {
    return null;
  }
}

function writeMemo(value: string | null): void {
  try {
    if (typeof localStorage === "undefined") return;
    if (value === null) localStorage.removeItem(IDENTIFIED_KEY);
    else localStorage.setItem(IDENTIFIED_KEY, value);
  } catch {
    // Storage unusable: the next identify is sent again, which is harmless.
  }
}

/**
 * Send `identify` for the current visitor. Resolves to the server's answer,
 * or to a local one when nothing was sent (`cookieless_project`,
 * `already_sent`, `invalid_user_id`, `network_error`). Never throws.
 */
export async function identifyVisitor(options: IdentifyVisitorOptions): Promise<IdentifyResult> {
  const { basePath, ingestKey, userId, traits, ready } = options;
  if (isCookielessIdentity()) {
    return {
      applied: false,
      outcome: "cookieless_project",
      message: "Identify stores nothing in cookieless mode.",
    };
  }
  if (typeof userId !== "string" || userId.trim() === "") {
    return { applied: false, outcome: "invalid_user_id", message: "identify() needs a non-empty user id." };
  }
  const memo = memoFor(userId, traits);
  if (readMemo() === memo) {
    return { applied: true, outcome: "already_sent", message: "This visitor already has this identity." };
  }
  try {
    await ready;
  } catch {
    // A failed page view does not stop identify; the server says if the
    // visitor is missing.
  }
  try {
    const response = await fetch(`${basePath}/${IDENTIFY_ENDPOINT}`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...ingestKeyHeaders(ingestKey) },
      body: JSON.stringify({ user_id: userId, traits, visitor_id: getOrCreateVisitorId() }),
    });
    const body = (await response.json().catch(() => null)) as Partial<IdentifyResult> | null;
    const result: IdentifyResult = {
      applied: Boolean(body?.applied),
      outcome: typeof body?.outcome === "string" ? body.outcome : `http_${response.status}`,
      message: typeof body?.message === "string" ? body.message : response.statusText,
    };
    if (response.ok && APPLIED_OUTCOMES.has(result.outcome)) writeMemo(memo);
    return result;
  } catch (error) {
    // eslint-disable-next-line no-console
    console.error("Failed to send identify:", error);
    return { applied: false, outcome: "network_error", message: String(error) };
  }
}

/**
 * Forget who this browser belongs to, for a sign-out: deletes the stored
 * visitor and session ids and the identify memo, and asks the server to
 * expire its tracking cookies. The next event starts a new, anonymous
 * visitor. Never throws.
 */
export async function resetVisitor(options: { basePath: string; ingestKey?: string }): Promise<void> {
  writeMemo(null);
  clearStoredIdentity();
  // A replay in progress belongs to the previous visitor; end it now.
  notifyIdentityReset();
  if (isCookielessIdentity()) return;
  try {
    await fetch(`${options.basePath}/${RESET_ENDPOINT}`, {
      method: "POST",
      headers: ingestKeyHeaders(options.ingestKey),
    });
  } catch (error) {
    // eslint-disable-next-line no-console
    console.error("Failed to reset analytics identity:", error);
  }
}
