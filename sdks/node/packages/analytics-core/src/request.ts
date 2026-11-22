// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** How long a same-origin flags or surveys request may take, body included. */
export const REQUEST_TIMEOUT_MS = 10_000;

/**
 * POST `body` as JSON and parse the JSON answer, failing after `timeoutMs`
 * whether the connection stalls before the headers or while the body is
 * read. The request is aborted when the platform supports it; the deadline
 * holds either way.
 */
export async function postJson<T>(
  fetchImpl: typeof fetch,
  url: string,
  body: string,
  timeoutMs: number = REQUEST_TIMEOUT_MS
): Promise<T> {
  const controller = typeof AbortController === "function" ? new AbortController() : undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      controller?.abort();
      reject(new Error(`${url} did not answer within ${Math.round(timeoutMs / 1000)}s`));
    }, timeoutMs);
  });
  const exchange = (async () => {
    const response = await fetchImpl(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      credentials: "same-origin",
      body,
      signal: controller?.signal,
    });
    if (!response.ok) throw new Error(`${url} returned ${response.status}`);
    return (await response.json()) as T;
  })();
  try {
    return await Promise.race([exchange, deadline]);
  } finally {
    clearTimeout(timer);
    // The losing side's rejection is handled here, not reported as unhandled.
    exchange.catch(() => undefined);
  }
}
