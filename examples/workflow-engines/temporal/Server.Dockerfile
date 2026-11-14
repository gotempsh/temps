# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
FROM temporalio/server:1.30.1@sha256:b15a5c64019a2338a87ba463812e3ebf9102053bfd7ba7359b9854b1772ea7bb
COPY dynamic.yaml /etc/temporal/config/dynamicconfig/docker.yaml
