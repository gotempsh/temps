# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Zero-dependency image: no package install, so the build only measures the
# git clone + docker build path the first-run suite is checking.
FROM node:22-alpine

WORKDIR /app
COPY server.js ./

ENV PORT=8080
EXPOSE 8080
USER node

CMD ["node", "server.js"]
