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
  const { trackEvent, enabled } = useTempsAnalytics();
  const trackRef = useRef(trackEvent);
  trackRef.current = trackEvent;
  const surveysRef = useRef<Surveys | null>(null);
  const openRef = useRef<(() => void) | null>(null);

  // One client per mounted component; the context is read once, like the
  // flags client.
  if (surveysRef.current === null && enabled) {
    surveysRef.current = new Surveys({
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
    if (!surveys) return undefined;
    let cancelled = false;

    const show = async (event?: string) => {
      if (openRef.current) return;
      await surveys.load();
      if (cancelled || openRef.current) return;
      const path = pathname ?? window.location.pathname;
      const next = surveys.eligible(path, event)[0];
      if (!next) return;
      surveys.markShown(next);
      const close = renderSurveyWidget(next, {
        onSubmit: (answers) => {
          const result = surveys.submit(next, answers);
          if (result.ok) openRef.current = null;
          return result.ok ? null : result.errors;
        },
        onDismiss: () => {
          openRef.current = null;
          surveys.dismiss(next);
        },
      });
      openRef.current = () => {
        openRef.current = null;
        close();
      };
    };

    void show();
    const onTrack = (event: Event) => {
      const name = (event as CustomEvent<{ name?: string }>).detail?.name;
      if (typeof name === "string") void show(name);
    };
    const onPop = () => void show();
    window.addEventListener(TRACK_EVENT, onTrack);
    window.addEventListener("popstate", onPop);
    return () => {
      cancelled = true;
      window.removeEventListener(TRACK_EVENT, onTrack);
      window.removeEventListener("popstate", onPop);
    };
  }, [pathname, enabled]);

  // Close an open survey when the component goes away.
  useEffect(() => () => openRef.current?.(), []);

  return null;
}
