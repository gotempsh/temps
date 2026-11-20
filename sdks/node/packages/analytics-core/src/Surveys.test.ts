// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, it, vi } from "vitest";
import { pathMatches, Surveys, validateAnswers, type Survey } from "./Surveys";
import { renderSurveyWidget } from "./SurveyWidget";

function served(surveys: Survey[], status = 200) {
  return vi.fn(async () =>
    new Response(JSON.stringify({ surveys }), {
      status,
      headers: { "Content-Type": "application/json" },
    })
  );
}

function memoryStorage() {
  const data = new Map<string, string>();
  return {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => {
      data.set(key, value);
    },
  };
}

const nps: Survey = {
  id: 1,
  name: "How are we doing?",
  questions: [
    { id: "score", kind: "nps", prompt: "How likely are you to recommend us?" },
    { id: "why", kind: "open_text", prompt: "Why?", optional: true },
  ],
  display: { once_per_visitor: true },
};

const checkout: Survey = {
  id: 2,
  name: "Checkout",
  questions: [{ id: "plan", kind: "single_choice", prompt: "Which plan?", choices: ["Free", "Pro"] }],
  display: { path_pattern: "/checkout/*", trigger_event: "checkout_completed", once_per_visitor: false },
};

describe("pathMatches", () => {
  it("matches whole paths with * wildcards", () => {
    expect(pathMatches("/pricing", "/pricing")).toBe(true);
    expect(pathMatches("/pricing", "/pricing/team")).toBe(false);
    expect(pathMatches("/checkout/*", "/checkout/step-2")).toBe(true);
    expect(pathMatches("/checkout/*", "/checkout")).toBe(false);
    expect(pathMatches("/docs/*/api", "/docs/v2/api")).toBe(true);
    expect(pathMatches("/docs/*/api", "/docs/v2/api/extra")).toBe(false);
    expect(pathMatches("*", "/anything")).toBe(true);
    // The prefix and suffix may not overlap.
    expect(pathMatches("/a*a", "/a")).toBe(false);
  });
});

describe("validateAnswers", () => {
  it("accepts valid answers and drops answers to unknown questions", () => {
    const result = validateAnswers(nps, { score: 9, ghost: "x" });
    expect(result).toEqual({ ok: true, responses: { score: 9 } });
  });

  it("reports a missing required answer and out-of-range scores", () => {
    expect(validateAnswers(nps, { why: "fine" })).toEqual({
      ok: false,
      errors: { score: "This question needs an answer." },
    });
    const bad = validateAnswers(nps, { score: 11 });
    expect(bad.ok).toBe(false);
  });

  it("checks choices and de-duplicates multiple choice", () => {
    const survey: Survey = {
      ...checkout,
      questions: [
        { id: "plan", kind: "single_choice", prompt: "Plan?", choices: ["Free", "Pro"] },
        { id: "tags", kind: "multiple_choice", prompt: "Tags?", choices: ["a", "b"] },
      ],
    };
    expect(validateAnswers(survey, { plan: "Gold", tags: ["a"] }).ok).toBe(false);
    expect(validateAnswers(survey, { plan: "Pro", tags: ["a", "a", "b"] })).toEqual({
      ok: true,
      responses: { plan: "Pro", tags: ["a", "b"] },
    });
  });

  it("cuts open text to the length the server keeps", () => {
    const result = validateAnswers(nps, { score: 3, why: "x".repeat(3000) });
    expect(result.ok && (result.responses.why as string).length).toBe(2000);
  });
});

describe("Surveys", () => {
  it("fetches once from the same-origin endpoint", async () => {
    const fetch = served([nps]);
    const surveys = new Surveys({ fetch, onEvent: () => undefined, storage: memoryStorage(), key: "user-1" });
    await Promise.all([surveys.load(), surveys.load()]);
    expect(fetch).toHaveBeenCalledTimes(1);
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/_temps/surveys");
    expect(JSON.parse(init.body as string)).toEqual({ key: "user-1", attributes: {} });
    expect(surveys.all().map((s) => s.id)).toEqual([1]);
  });

  it("applies the path, trigger and once-per-visitor conditions", async () => {
    const storage = memoryStorage();
    const surveys = new Surveys({ fetch: served([nps, checkout]), onEvent: () => undefined, storage });
    await surveys.load();
    expect(surveys.eligible("/").map((s) => s.id)).toEqual([1]);
    // Event-triggered surveys only appear for their event, on their path.
    expect(surveys.eligible("/checkout/done", "checkout_completed").map((s) => s.id)).toEqual([2]);
    expect(surveys.eligible("/other", "checkout_completed")).toEqual([]);
    expect(surveys.eligible("/checkout/done", "signup")).toEqual([]);

    surveys.markShown(nps);
    expect(surveys.eligible("/")).toEqual([]);
    // A later page load (a new instance) still remembers the visitor saw it.
    const again = new Surveys({ fetch: served([nps]), onEvent: () => undefined, storage });
    await again.load();
    expect(again.eligible("/")).toEqual([]);
  });

  it("records shown, dismissed and sent events with the survey id", async () => {
    const onEvent = vi.fn();
    const surveys = new Surveys({ fetch: served([nps]), onEvent, storage: memoryStorage() });
    surveys.markShown(nps);
    surveys.dismiss(nps);
    expect(surveys.submit(nps, {})).toEqual({ ok: false, errors: { score: "This question needs an answer." } });
    expect(surveys.submit(nps, { score: 10, why: "great" })).toEqual({ ok: true });
    expect(onEvent.mock.calls).toEqual([
      ["survey_shown", { survey_id: 1, survey_name: "How are we doing?" }],
      ["survey_dismissed", { survey_id: 1, survey_name: "How are we doing?" }],
      ["survey_sent", { survey_id: 1, survey_name: "How are we doing?", responses: { score: 10, why: "great" } }],
    ]);
  });

  it("shows nothing when the endpoint fails", async () => {
    const onError = vi.fn();
    const surveys = new Surveys({ fetch: served([], 503), onEvent: () => undefined, onError, storage: memoryStorage() });
    await surveys.load();
    expect(surveys.status).toBe("error");
    expect(surveys.eligible("/")).toEqual([]);
    expect(onError).toHaveBeenCalledOnce();
  });

  it("never fetches when disabled", async () => {
    const fetch = served([nps]);
    const surveys = new Surveys({ fetch, onEvent: () => undefined, disabled: true });
    await surveys.load();
    expect(fetch).not.toHaveBeenCalled();
  });
});

describe("renderSurveyWidget", () => {
  afterEach(() => {
    document.body.replaceChildren();
  });

  function shadow(): ShadowRoot {
    const host = document.querySelector("[data-temps-survey]");
    if (!host?.shadowRoot) throw new Error("widget not rendered");
    return host.shadowRoot;
  }

  it("renders an accessible dialog and submits the picked answers", () => {
    const onSubmit = vi.fn(() => null);
    renderSurveyWidget(nps, { onSubmit, onDismiss: () => undefined });
    const root = shadow();
    const dialog = root.querySelector("[role=dialog]");
    expect(dialog?.getAttribute("aria-labelledby")).toBe("temps-survey-1-title");
    expect(root.querySelectorAll("input[type=radio]")).toHaveLength(11);

    (root.querySelector("input[value='9']") as HTMLInputElement).checked = true;
    (root.querySelector("textarea") as HTMLTextAreaElement).value = "fast";
    (root.querySelector("form") as HTMLFormElement).dispatchEvent(new Event("submit", { cancelable: true }));
    expect(onSubmit).toHaveBeenCalledWith({ score: 9, why: "fast" });
    expect(root.querySelector("[role=status]")?.textContent).toBe("Thanks for your answer.");
  });

  it("shows the errors it is given and keeps the form", () => {
    renderSurveyWidget(nps, {
      onSubmit: () => ({ score: "This question needs an answer." }),
      onDismiss: () => undefined,
    });
    const root = shadow();
    (root.querySelector("form") as HTMLFormElement).dispatchEvent(new Event("submit", { cancelable: true }));
    expect(root.querySelector("[role=alert]")?.textContent).toBe("This question needs an answer.");
    expect(root.querySelector("button[type=submit]")).not.toBeNull();
  });

  it("dismisses on close and on Escape", () => {
    const onDismiss = vi.fn();
    renderSurveyWidget(nps, { onSubmit: () => null, onDismiss });
    (shadow().querySelector("button[aria-label='Close survey']") as HTMLButtonElement).click();
    expect(onDismiss).toHaveBeenCalledOnce();
    expect(document.querySelector("[data-temps-survey]")).toBeNull();

    renderSurveyWidget(checkout, { onSubmit: () => null, onDismiss });
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(onDismiss).toHaveBeenCalledTimes(2);
    expect(document.querySelector("[data-temps-survey]")).toBeNull();
  });
});
