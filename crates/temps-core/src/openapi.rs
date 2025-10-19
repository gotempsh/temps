//! OpenAPI schema utilities for merging multiple API documentation schemas

use utoipa::openapi::OpenApi;

/// Merges multiple OpenAPI schemas into a single schema.
///
/// This function takes a base schema and merges additional schemas into it,
/// combining paths, components, tags, and other OpenAPI elements.
///
/// # Arguments
///
/// * `base` - The base OpenAPI schema to merge into
/// * `schemas` - Additional schemas to merge into the base
///
/// # Returns
///
/// A new OpenApi instance with all schemas merged
///
/// # Example
///
/// ```rust
/// use utoipa::OpenApi;
/// use temps_core::openapi::merge_openapi_schemas;
///
/// #[derive(OpenApi)]
/// #[openapi(paths(my_handler))]
/// struct ApiDoc1;
///
/// #[derive(OpenApi)]
/// #[openapi(paths(other_handler))]
/// struct ApiDoc2;
///
/// let merged = merge_openapi_schemas(
///     ApiDoc1::openapi(),
///     vec![ApiDoc2::openapi()]
/// );
/// ```
pub fn merge_openapi_schemas(mut base: OpenApi, schemas: Vec<OpenApi>) -> OpenApi {
    for schema in schemas {
        // Merge paths
        base.paths.paths.extend(schema.paths.paths);

        // Merge components
        if let Some(components) = schema.components {
            let base_components = base.components.get_or_insert_with(Default::default);

            // Merge schemas
            base_components.schemas.extend(components.schemas);

            // Merge responses
            base_components.responses.extend(components.responses);

            // Merge security schemes
            base_components.security_schemes.extend(components.security_schemes);
        }

        // Merge tags
        if let Some(tags) = schema.tags {
            let base_tags = base.tags.get_or_insert_with(Vec::new);
            base_tags.extend(tags);
        }

        // Merge servers
        if let Some(servers) = schema.servers {
            let base_servers = base.servers.get_or_insert_with(Vec::new);
            base_servers.extend(servers);
        }

        // Merge security requirements
        if let Some(security) = schema.security {
            let base_security = base.security.get_or_insert_with(Vec::new);
            base_security.extend(security);
        }

        // Merge external docs
        if schema.external_docs.is_some() && base.external_docs.is_none() {
            base.external_docs = schema.external_docs;
        }
    }

    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use utoipa::openapi::{
        OpenApiBuilder, PathItem, PathsBuilder, InfoBuilder,
        HttpMethod, path::OperationBuilder
    };

    #[test]
    fn test_merge_empty_schemas() {
        let base = OpenApiBuilder::new()
            .info(InfoBuilder::new()
                .title("Test API")
                .version("1.0.0")
                .build())
            .paths(PathsBuilder::new().build())
            .build();

        let result = merge_openapi_schemas(base.clone(), vec![]);
        assert_eq!(result.info.title, "Test API");
    }

    #[test]
    fn test_merge_paths() {
        let test_operation = OperationBuilder::new()
            .summary(Some("Test endpoint"))
            .build();

        let other_operation = OperationBuilder::new()
            .summary(Some("Other endpoint"))
            .build();

        let base = OpenApiBuilder::new()
            .info(InfoBuilder::new()
                .title("Test API")
                .version("1.0.0")
                .build())
            .paths(PathsBuilder::new()
                .path("/api/v1/test", PathItem::new(HttpMethod::Get, test_operation))
                .build())
            .build();

        let other = OpenApiBuilder::new()
            .info(InfoBuilder::new()
                .title("Other API")
                .version("1.0.0")
                .build())
            .paths(PathsBuilder::new()
                .path("/api/v1/other", PathItem::new(HttpMethod::Get, other_operation))
                .build())
            .build();

        let result = merge_openapi_schemas(base, vec![other]);

        assert!(result.paths.paths.contains_key("/api/v1/test"));
        assert!(result.paths.paths.contains_key("/api/v1/other"));
    }
}
