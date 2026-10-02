// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useEffect, useMemo, useRef } from "react";
import {
  SessionRecorder as CoreSessionRecorder,
  type SessionRecorderOptions,
} from "@temps-sdk/analytics-core";

export const SESSION_RECORDER_ENDPOINT = "session-replay";

/** rrweb sampling options, typed for React callers. */
export interface SessionRecordingSampling {
  scroll?: number;
  media?: number;
  mouseInteraction?: boolean | {
    click?: boolean;
    dblclick?: boolean;
    contextmenu?: boolean;
    focus?: boolean;
    blur?: boolean;
    touchstart?: boolean;
    touchend?: boolean;
    touchcancel?: boolean;
    play?: boolean;
    pause?: boolean;
  };
  mousemove?: boolean | number;
  input?: "all" | "last";
  canvas?: number | "all";
}

/**
 * Every recorder option `<SessionRecorder>` and the provider's
 * `sessionRecordingConfig` accept: the shared `SessionRecordingConfig` plus
 * the rrweb-level options of the core recorder (`maskInputOptions`,
 * `blockSelector`, `ignoreSelector`, `slimDOMOptions`, `sampling`).
 *
 * Derived from the core recorder's options rather than re-declared: a
 * hand-copied field list is how the provider came to drop `idleTimeout`,
 * `useDefaultExcludedPaths` and others without an error.
 */
export type SessionRecorderConfig = Omit<
  SessionRecorderOptions,
  "basePath" | "ingestKey" | "domain" | "enabled" | "sampling"
> & {
  sampling?: SessionRecordingSampling;
};

export type SessionRecorderProps = SessionRecorderConfig & {
  basePath: string; // Required, no default
  /**
   * Analytics ingest key (`pa_…`). Required only when Temps does not serve or
   * proxy the app. See `AnalyticsClientOptions.ingestKey`.
   */
  ingestKey?: string;
  domain?: string;
  enabled?: boolean;
};

/**
 * React binding for the framework-agnostic recorder in `@temps-sdk/analytics-core`.
 *
 * This used to be a second, independent rrweb implementation. Keeping two
 * copies meant every ingest fix had to be made twice and they drifted instead
 * — batch handling, the start-up race, and idle gating were all fixed in one
 * and not the other. The component now owns only React lifecycle; all
 * recording behaviour lives in the core class.
 */
export function SessionRecorder(props: SessionRecorderProps): null {
  const recorderRef = useRef<CoreSessionRecorder | null>(null);

  // Object and array props are rebuilt on every render by the caller (and by
  // this component's own defaults), so depending on them directly would tear
  // down and re-create the recorder on each render — losing the session and
  // re-snapshotting the DOM every time. Key the effect on the serialized
  // options instead.
  const optionsKey = useMemo(() => JSON.stringify(props), [props]);

  useEffect(() => {
    if (!props.enabled) return;

    const { enabled: _enabled, sampling, ...options } = props;
    const recorder = new CoreSessionRecorder({
      ...options,
      sampling: sampling as Record<string, unknown> | undefined,
      enabled: true,
    });
    recorderRef.current = recorder;

    return () => {
      recorder.destroy();
      recorderRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [optionsKey]);

  return null;
}
