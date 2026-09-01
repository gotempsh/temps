// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useEffect, useMemo, useRef } from "react";
import { SessionRecorder as CoreSessionRecorder } from "@temps-sdk/analytics-core";

export const SESSION_RECORDER_ENDPOINT = "session-replay";

interface SessionRecorderProps {
  basePath: string;  // Required, no default
  domain?: string;
  enabled?: boolean;
  excludedPaths?: string[];
  /**
   * Merge the built-in sensitive-path defaults (login, checkout, payment, …)
   * with `excludedPaths`. Defaults to true.
   */
  useDefaultExcludedPaths?: boolean;
  sessionSampleRate?: number;
  maskAllInputs?: boolean;
  maskTextSelector?: string;
  blockClass?: string;
  ignoreClass?: string;
  maskTextClass?: string;
  recordCanvas?: boolean;
  collectFonts?: boolean;
  slimDOMOptions?: Record<string, boolean>;
  maskInputOptions?: {
    password?: boolean;
    email?: boolean;
  };
  /**
   * Number of events to batch before sending. Default: 100
   * Events are sent when EITHER batchSize is reached OR flushInterval elapses (whichever comes first).
   */
  batchSize?: number;
  /**
   * Interval in milliseconds to flush events. Default: 10000 (10s)
   * Events are sent when EITHER flushInterval elapses OR batchSize is reached (whichever comes first).
   */
  flushInterval?: number;
  /**
   * Milliseconds of no interaction after which recording pauses. rrweb records
   * DOM mutations rather than user actions, so without this a page that
   * animates or mutates on a timer keeps producing events from a visitor who
   * is doing nothing. Default: 60000. Set to 0 to never pause.
   */
  idleTimeout?: number;
  /** Pause recording while the tab is hidden. Default: true. */
  pauseOnHidden?: boolean;
  /** Force a full DOM snapshot every N ms. Default: 30000. */
  checkoutEveryNms?: number;
  /** Force a full DOM snapshot every N events. Default: 200. */
  checkoutEveryNth?: number;
  /** Cap on events buffered while ingest is unreachable. Default: 5000. */
  maxBufferedEvents?: number;
  ignoreSelector?: string;
  blockSelector?: string;
  sampling?: {
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
  };
}

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

    const recorder = new CoreSessionRecorder({
      basePath: props.basePath,
      domain: props.domain,
      enabled: true,
      excludedPaths: props.excludedPaths,
      useDefaultExcludedPaths: props.useDefaultExcludedPaths,
      sessionSampleRate: props.sessionSampleRate,
      maskAllInputs: props.maskAllInputs,
      maskTextSelector: props.maskTextSelector,
      blockClass: props.blockClass,
      ignoreClass: props.ignoreClass,
      maskTextClass: props.maskTextClass,
      ignoreSelector: props.ignoreSelector,
      blockSelector: props.blockSelector,
      recordCanvas: props.recordCanvas,
      collectFonts: props.collectFonts,
      slimDOMOptions: props.slimDOMOptions,
      maskInputOptions: props.maskInputOptions,
      batchSize: props.batchSize,
      flushInterval: props.flushInterval,
      idleTimeout: props.idleTimeout,
      pauseOnHidden: props.pauseOnHidden,
      checkoutEveryNms: props.checkoutEveryNms,
      checkoutEveryNth: props.checkoutEveryNth,
      maxBufferedEvents: props.maxBufferedEvents,
      sampling: props.sampling as Record<string, unknown> | undefined,
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
