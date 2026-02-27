//! PII field path resolution from OpenAPI schemas.
//!
//! Given a parsed [`openapiv3::OpenAPI`] document, this module produces the set of
//! JSON pointer paths (dot-notation) to properties annotated with `x-pii: true`.

use std::collections::HashSet;

use openapiv3::{OpenAPI, Schema, SchemaKind, Type};

/// A set of dot-notation field paths that are marked as PII in the schema.
///
/// Example paths: `"ssn"`, `"user.address.zip"`, `"orders[].card_number"`.
pub type PiiFieldPaths = HashSet<String>;

/// Walk an [`OpenAPI`] document and collect all dot-notation paths to properties
/// marked `x-pii: true`.
///
/// The walk starts at every schema defined in `components/schemas` and recurses
/// into nested object properties. Array items are represented with the `[]` suffix
/// on the array field name (e.g. `"orders[].card_number"`).
pub fn resolve_pii_paths(api: &OpenAPI) -> PiiFieldPaths {
    let mut paths = HashSet::new();

    let components = match &api.components {
        Some(c) => c,
        None => return paths,
    };

    for (_name, schema_ref) in &components.schemas {
        if let openapiv3::ReferenceOr::Item(schema) = schema_ref {
            walk_schema(schema, "", &mut paths);
        }
    }

    paths
}

/// Recursively walk a [`Schema`], appending discovered PII paths to `out`.
fn walk_schema(schema: &Schema, prefix: &str, out: &mut PiiFieldPaths) {
    match &schema.schema_kind {
        SchemaKind::Type(Type::Object(obj)) => {
            for (prop_name, prop_ref) in &obj.properties {
                let path = if prefix.is_empty() {
                    prop_name.clone()
                } else {
                    format!("{prefix}.{prop_name}")
                };

                if let openapiv3::ReferenceOr::Item(prop_schema) = prop_ref {
                    // Check `x-pii: true` in the extension map.
                    let is_pii = prop_schema
                        .schema_data
                        .extensions
                        .get("x-pii")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    if is_pii {
                        out.insert(path.clone());
                    }

                    // Recurse into nested objects.
                    walk_schema(prop_schema, &path, out);
                }
            }
        }
        SchemaKind::Type(Type::Array(arr)) => {
            // Arrays: append `[]` and walk the items schema.
            if let Some(openapiv3::ReferenceOr::Item(items)) = &arr.items {
                let array_path = if prefix.is_empty() {
                    "[]".to_string()
                } else {
                    format!("{prefix}[]")
                };
                walk_schema(items, &array_path, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_api(yaml: &str) -> OpenAPI {
        serde_yaml::from_str(yaml).expect("valid YAML")
    }

    #[test]
    fn flat_pii_field_detected() {
        let yaml = r#"
openapi: "3.0.0"
info:
  title: test
  version: "1"
paths: {}
components:
  schemas:
    Person:
      type: object
      properties:
        name:
          type: string
        ssn:
          type: string
          x-pii: true
"#;
        let api = parse_api(yaml);
        let paths = resolve_pii_paths(&api);
        assert!(paths.contains("ssn"), "expected 'ssn' in {paths:?}");
        assert!(!paths.contains("name"), "unexpected 'name' in {paths:?}");
    }

    #[test]
    fn nested_pii_field_detected() {
        let yaml = r#"
openapi: "3.0.0"
info:
  title: test
  version: "1"
paths: {}
components:
  schemas:
    User:
      type: object
      properties:
        address:
          type: object
          properties:
            zip:
              type: string
              x-pii: true
            city:
              type: string
"#;
        let api = parse_api(yaml);
        let paths = resolve_pii_paths(&api);
        assert!(paths.contains("address.zip"), "{paths:?}");
        assert!(!paths.contains("address.city"), "{paths:?}");
    }

    #[test]
    fn no_components_returns_empty() {
        let yaml = r#"
openapi: "3.0.0"
info:
  title: test
  version: "1"
paths: {}
"#;
        let api = parse_api(yaml);
        let paths = resolve_pii_paths(&api);
        assert!(paths.is_empty());
    }
}
