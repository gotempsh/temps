// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Cookieless mode is one-way for the page's lifetime, so this test lives in
// its own file: vitest gives every file a fresh module registry.

import { describe, expect, it, vi } from "vitest";
import { enableCookielessIdentity } from "./identity";
import { Surveys, type Survey } from "./Surveys";

const nps: Survey = {
  id: 1,
  name: "How are we doing?",
  questions: [{ id: "score", kind: "nps", prompt: "How likely are you to recommend us?" }],
  display: { once_per_visitor: true },
};

function served() {
  return vi.fn(
    async () =>
      new Response(JSON.stringify({ surveys: [nps] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })
  );
}

describe("surveys in cookieless mode (ADR 109 D3)", () => {
  it("keeps once-per-visitor in memory and never touches device storage", async () => {
    enableCookielessIdentity();
    const storage = { getItem: vi.fn(() => "[1]"), setItem: vi.fn() };

    const surveys = new Surveys({ fetch: served(), onEvent: () => undefined, storage });
    await surveys.load();
    // A list an earlier, persistent visit stored is not read.
    expect(surveys.eligible("/").map((s) => s.id)).toEqual([1]);
    surveys.markShown(nps);
    // Still once per page...
    expect(surveys.eligible("/")).toEqual([]);
    expect(storage.getItem).not.toHaveBeenCalled();
    expect(storage.setItem).not.toHaveBeenCalled();

    // ...but a new page load cannot know the visitor saw it.
    const again = new Surveys({ fetch: served(), onEvent: () => undefined, storage });
    await again.load();
    expect(again.eligible("/").map((s) => s.id)).toEqual([1]);
    expect(storage.setItem).not.toHaveBeenCalled();
  });
});
