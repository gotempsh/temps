// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const IMMUTABLE_MARIADB_IMAGE = /^(?:sha256:[0-9a-fA-F]{64}|[^@\s]+@sha256:[0-9a-fA-F]{64})$/

/**
 * Build the service parameters used by the live recovery scenario.
 *
 * MariaDB is deliberately stricter than the other managed-service fixtures:
 * its WAL-G image contains the physical backup tools, so accepting a mutable
 * tag would make a green recovery run non-reproducible. CI passes Docker's
 * content-addressed local image ID; published runs may pass a repository
 * digest instead.
 */
export function mariadbServiceParameters(
  mariadbImage: string | undefined,
  database: string,
): {
  database: string
  username: string
  docker_image: string
} {
  const dockerImage = mariadbImage?.trim()
  if (!dockerImage || !IMMUTABLE_MARIADB_IMAGE.test(dockerImage)) {
    throw new Error(
      'MariaDB recovery scenario requires --mariadb-image (or TEMPS_E2E_MARIADB_IMAGE) ' +
        'as repository@sha256:<64-hex-digest> or a local sha256:<64-hex-image-id>',
    )
  }

  return {
    database,
    username: 'app',
    docker_image: dockerImage,
  }
}
