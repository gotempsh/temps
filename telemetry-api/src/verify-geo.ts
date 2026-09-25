// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Build-time check (run by the Dockerfile): fail the image build when the
// GeoLite2 DB is missing or unusable, using the same probe the server runs at
// startup — so a checkout without the gitignored DB fails at build, not deploy.

import { initGeo } from "./geo.js";
import { errorFields, log } from "./log.js";

try {
  await initGeo({ required: true });
} catch (err) {
  log("error", "geo", "GeoLite2 build-time verification failed", errorFields(err));
  process.exit(1);
}
