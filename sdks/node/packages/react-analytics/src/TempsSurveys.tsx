// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useEffect, useRef } from "react";
import {
  renderSurveyWidget,
  Surveys,
  type SurveysOptions,
} from "@temps-sdk/analytics-core";
import { TRACK_EVENT, useTempsAnalytics } from "./Provider";

export interface TempsSurveysProps extends Pick<SurveysOptions, "attributes" | "onError"> {
  /** Subject for sampling and flag conditions, e.g. a signed-in user id.
   * Defaults to the Temps visitor cookie. */
  visitorKey?: string;
  /**
   * The current path. Pass your router's (`usePathname()` in Next.js) so a
   * survey for `/checkout/*` appears on client-side navigation. Defaults to
   * `window.location.pathname`, re-read on back/forward navigation.
   */
  pathname?: string;
}

/**
 * Shows in-app surveys defined in the Temps console (ADR 110 D1) with the
 * built-in widget. Place it once inside `<TempsAnalyticsProvider>`:
 *
 * ```tsx
 * <TempsAnalyticsProvider>
 *   <App />
 *   <TempsSurveys pathname={usePathname()} />
 * </TempsAnalyticsProvider>
 * ```
 *
 * Answers are recorded as `survey_shown`, `survey_dismissed` and
 * `survey_sent` analytics events, so nothing is shown while analytics is
 * disabled (for example on localhost unless `ignoreLocalhost={false}`).
 */
export function TempsSurveys({ pathname, visitorKey, attributes, onError }: TempsSurveysProps): null {
  const { trackEvent, enabled, basePath } = useTempsAnalytics();
  const trackRef = useRef(trackEvent);
  trackRef.current = trackEvent;
  const surveysRef = useRef<Surveys | null>(null);
  const openRef = useRef<(() => void) | null>(null);
  // The subject and attributes as a value, so a new object with the same
  // contents is not a change.
  const context = JSON.stringify([visitorKey ?? null, attributes ?? {}]);
  const appliedContextRef = useRef(context);

  // One client per mounted component. A later change of `visitorKey` or
  // `attributes` (a user signing in) is applied in the effect below.
  if (surveysRef.current === null && enabled) {
    appliedContextRef.current = context;
    surveysRef.current = new Surveys({
      // The provider's path, so delivery and the answers' events agree.
      basePath,
      key: visitorKey,
      attributes,
      onError,
      onEvent: (name, data) => {
        void trackRef.current(name, data);
      },
    });
  }

  useEffect(() => {
    const surveys = surveysRef.current;
    // Analytics turned off: close what is open and listen for nothing, since
    // answers could not be recorded.
    if (!surveys || !enabled) {
      openRef.current?.();
      return undefined;
    }
    if (appliedContextRef.current !== context) {
      // Sampling and flag conditions depend on the subject: drop what was
      // fetched for the previous one and fetch again.
      appliedContextRef.current = context;
      surveys.setContext(visitorKey, attributes);
    }
    let cancelled = false;

    const show = async (event?: string): Promise<void> => {
      // Loaded even while a survey is open, so a new context is fetched at
      // once; `load()` is a no-op once loaded.
      await surveys.load();
      if (cancelled || openRef.current) return;
      const path = pathname ?? window.location.pathname;
      const next = surveys.eligible(path, event)[0];
      if (!next) return;
      surveys.markShown(next);
      // Owned until the card has left the page, thank-you message included,
      // so a later event cannot open a second one over it and unmounting
      // can still remove it.
      let closed = false;
      const close = renderSurveyWidget(next, {
        onSubmit: (answers) => {
          const result = surveys.submit(next, answers);
          return result.ok ? null : result.errors;
        },
        onDismiss: () => surveys.dismiss(next),
        onClosed: () => {
          closed = true;
          if (openRef.current === closeThis) openRef.current = null;
        },
      });
      const closeThis = (): void => close();
      if (!closed) openRef.current = closeThis;
    };

    void show();
    const onTrack = (event: Event): void => {
      const name = (event as CustomEvent<{ name?: string }>).detail?.name;
      if (typeof name === "string") void show(name);
    };
    const onPop = (): void => void show();
    window.addEventListener(TRACK_EVENT, onTrack);
    window.addEventListener("popstate", onPop);
    return () => {
      cancelled = true;
      window.removeEventListener(TRACK_EVENT, onTrack);
      window.removeEventListener("popstate", onPop);
    };
    // `context` stands for `visitorKey` and `attributes`, compared by value.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pathname, enabled, context]);

  // Close an open survey when the component goes away.
  useEffect(() => () => openRef.current?.(), []);

  return null;
}
