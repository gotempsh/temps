// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, waitFor } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider, useTempsAnalytics } from "../Provider";
import { TempsSurveys } from "../TempsSurveys";

const surveys = [
  {
    id: 1,
    name: "Quick question",
    questions: [{ id: "score", kind: "nps", prompt: "How likely are you to recommend us?" }],
    display: { once_per_visitor: false },
  },
  {
    id: 2,
    name: "After checkout",
    questions: [{ id: "plan", kind: "single_choice", prompt: "Which plan?", choices: ["Free", "Pro"] }],
    display: { trigger_event: "checkout_completed", once_per_visitor: false },
  },
];

function serve() {
  return vi.spyOn(global, "fetch").mockImplementation(async (input) => {
    if (String(input).endsWith("/surveys")) {
      return new Response(JSON.stringify({ surveys }), { status: 200 });
    }
    return new Response("ok", { status: 200 });
  });
}

function widget(id: number): ShadowRoot | null {
  return document.querySelector(`[data-temps-survey="${id}"]`)?.shadowRoot ?? null;
}

function eventNames(fetch: ReturnType<typeof serve>): string[] {
  return fetch.mock.calls
    .filter(([input]) => !String(input).endsWith("/surveys"))
    .map(([, init]) => {
      try {
        return (JSON.parse(String((init as RequestInit | undefined)?.body)) as { event_name?: string }).event_name ?? "";
      } catch {
        return "";
      }
    })
    .filter((name) => name.startsWith("survey_"));
}

afterEach(() => {
  vi.restoreAllMocks();
  document.body.replaceChildren();
  window.localStorage.clear();
});

describe("TempsSurveys", () => {
  it("shows a page-load survey and records that it was shown", async () => {
    const fetch = serve();
    render(
      <TempsAnalyticsProvider ignoreLocalhost={false} autoTrackPageviews={false} autoTrackEngagement={false} autoTrackSpeedAnalytics={false}>
        <TempsSurveys pathname="/" />
      </TempsAnalyticsProvider>
    );
    await waitFor(() => expect(widget(1)).not.toBeNull());
    await waitFor(() => expect(eventNames(fetch)).toContain("survey_shown"));
    expect(widget(2)).toBeNull();
  });

  it("shows an event-triggered survey when the app tracks that event", async () => {
    serve();
    let track: ((name: string) => Promise<void>) | null = null;
    function Capture() {
      const { trackEvent } = useTempsAnalytics();
      track = (name) => trackEvent(name);
      return null;
    }
    render(
      <TempsAnalyticsProvider ignoreLocalhost={false} autoTrackPageviews={false} autoTrackEngagement={false} autoTrackSpeedAnalytics={false}>
        <Capture />
        <TempsSurveys pathname="/checkout/done" />
      </TempsAnalyticsProvider>
    );
    // Close the page-load survey first: one survey is shown at a time.
    await waitFor(() => expect(widget(1)).not.toBeNull());
    act(() => {
      (widget(1)?.querySelector("button[aria-label='Close survey']") as HTMLButtonElement).click();
    });
    await act(async () => {
      await track?.("checkout_completed");
    });
    await waitFor(() => expect(widget(2)).not.toBeNull());
  });

  it("shows nothing while analytics is disabled", async () => {
    const fetch = serve();
    render(
      <TempsAnalyticsProvider disabled>
        <TempsSurveys pathname="/" />
      </TempsAnalyticsProvider>
    );
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(fetch.mock.calls.some(([input]) => String(input).endsWith("/surveys"))).toBe(false);
    expect(widget(1)).toBeNull();
  });
});
