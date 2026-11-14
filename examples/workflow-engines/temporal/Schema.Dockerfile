# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
FROM temporalio/admin-tools:1.30.1@sha256:4db77a7fd8a7663dfabce417d861117ad82ab88edc1bb8667d3b317d50d42cd3
COPY schema.sh /schema.sh
ENTRYPOINT ["/bin/sh", "/schema.sh"]
