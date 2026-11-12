// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

interface ConfigurableClient<C extends object> {
  getConfig(): C
  setConfig(config: C): unknown
}

/**
 * Puts a shared API client back to the `original` config a test file saved.
 *
 * `setConfig` merges into the current config, so `setConfig(original)` cannot
 * remove a key the test added: a fake `fetch` installed on the client would
 * outlive the test and answer the next file's requests, ahead of the
 * `globalThis.fetch` that file installed. Every key the original did not have
 * is cleared as well.
 */
export function restoreClientConfig<C extends object>(
  client: ConfigurableClient<C>,
  original: C
) {
  const added = Object.keys(client.getConfig()).filter(
    (key) => !(key in original)
  )
  client.setConfig({
    ...Object.fromEntries(added.map((key) => [key, undefined])),
    ...original,
  } as C)
}
