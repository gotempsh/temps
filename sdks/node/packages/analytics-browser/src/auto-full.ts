// Full build entry: force bun to inline rrweb + @rrweb/packer into this single-file IIFE.
//
// `@temps-sdk/analytics-core`'s barrel (index.ts) statically re-exports SessionRecorder, so
// importing it here makes SessionRecorder (and therefore rrweb/@rrweb/packer) statically
// reachable from the entry. bun then inlines rrweb; the dynamic import("./SessionRecorder")
// inside Analytics and the dynamic import("rrweb")/import("@rrweb/packer") inside
// SessionRecorder all resolve to the already-bundled copy (rewritten to
// Promise.resolve().then(), zero dangling import()) -> recording is fully functional here.
//
// This build is published WITHOUT the --external flags, so rrweb stays inlined (unlike the
// light build which strips it via --external).
import { SessionRecorder } from "@temps-sdk/analytics-core";
void SessionRecorder; // reference the value so it isn't tree-shaken before inlining

import "./auto"; // identical readDataset()+boot() auto-init path as the light build
