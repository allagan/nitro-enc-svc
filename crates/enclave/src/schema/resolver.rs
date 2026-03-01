//! PII field path resolution from OpenAPI schemas.
//!
//! Given a parsed [`openapiv3::OpenAPI`] document, this module produces the set of
//! JSON pointer paths (dot-notation) to properties annotated with `x-pii: true`.

use std::collections::HashSet;

use openapiv3::{OpenAPI, ReferenceOr, Schema, SchemaKind, Type};

/// A set of dot-notation field paths that are marked as PII in the schema.
///
/// Example paths: `"ssn"`, `"user.address.zip"`, `"orders[].card_number"`.
pub type PiiFieldPaths = HashSet<String>;

/// Walk an [`OpenAPI`] document and collect all dot-notation paths to properties
/// marked `x-pii: true`.
///
/// The walk starts at every schema defined in `components/schemas` and recurses
/// into nested object properties and array items. `$ref` references are resolved
/// against `components/schemas` and walked transitively.
///
/// Array items are represented with the `[]` suffix on the array field name
/// (e.g. `"orders[].card_number"`, `"AddressLine[]"` for an array of PII strings).
pub fn resolve_pii_paths(api: &OpenAPI) -> PiiFieldPaths {
    let mut paths = HashSet::new();

    let components = match &api.components {
        Some(c) => c,
        None => return paths,
    };

    for (_name, schema_ref) in &components.schemas {
        if let ReferenceOr::Item(schema) = schema_ref {
            walk_schema(api, schema, "", &mut paths);
        }
    }

    paths
}

/// Resolve a `$ref` string (e.g. `"#/components/schemas/Foo"`) to the
/// corresponding [`Schema`] in `components/schemas`.
///
/// Returns `None` if the reference format is not recognised, the schema is
/// absent, or the target is itself a `$ref` (chained references are not
/// supported).
fn resolve_ref<'a>(api: &'a OpenAPI, reference: &str) -> Option<&'a Schema> {
    let name = reference.strip_prefix("#/components/schemas/")?;
    match api.components.as_ref()?.schemas.get(name)? {
        ReferenceOr::Item(s) => Some(s),
        ReferenceOr::Reference { .. } => None,
    }
}

/// Recursively walk a [`Schema`], appending discovered PII paths to `out`.
///
/// - **Object properties**: each property is walked; `$ref` properties are
///   resolved via [`resolve_ref`] and walked transitively.
/// - **Array items**: if the items schema carries `x-pii: true` (e.g. an array
///   of PII strings), the array path itself (with `[]` suffix) is emitted.
///   Items are also walked recursively for arrays of objects with nested PII.
///   `$ref` items are resolved before walking.
fn walk_schema(api: &OpenAPI, schema: &Schema, prefix: &str, out: &mut PiiFieldPaths) {
    match &schema.schema_kind {
        SchemaKind::Type(Type::Object(obj)) => {
            for (prop_name, prop_ref) in &obj.properties {
                let path = if prefix.is_empty() {
                    prop_name.clone()
                } else {
                    format!("{prefix}.{prop_name}")
                };

                let resolved: Option<&Schema> = match prop_ref {
                    ReferenceOr::Item(s) => Some(s.as_ref()),
                    ReferenceOr::Reference { reference } => resolve_ref(api, reference),
                };

                if let Some(prop_schema) = resolved {
                    let is_pii = prop_schema
                        .schema_data
                        .extensions
                        .get("x-pii")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    if is_pii {
                        out.insert(path.clone());
                    }

                    walk_schema(api, prop_schema, &path, out);
                }
            }
        }
        SchemaKind::Type(Type::Array(arr)) => {
            if let Some(items_ref) = &arr.items {
                let array_path = if prefix.is_empty() {
                    "[]".to_string()
                } else {
                    format!("{prefix}[]")
                };

                let resolved: Option<&Schema> = match items_ref {
                    ReferenceOr::Item(s) => Some(s.as_ref()),
                    ReferenceOr::Reference { reference } => resolve_ref(api, reference),
                };

                if let Some(items_schema) = resolved {
                    // If the items themselves carry `x-pii: true` (e.g. an array
                    // of PII strings like AddressLine[]), emit the array path.
                    let items_is_pii = items_schema
                        .schema_data
                        .extensions
                        .get("x-pii")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    if items_is_pii {
                        out.insert(array_path.clone());
                    }

                    walk_schema(api, items_schema, &array_path, out);
                }
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

    // ── existing tests ────────────────────────────────────────────────────────

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

    // ── $ref resolution ───────────────────────────────────────────────────────

    /// A property declared as `$ref` to another schema must expose the referenced
    /// schema's PII fields prefixed with the property name.
    #[test]
    fn ref_property_pii_paths_resolved() {
        let yaml = r#"
openapi: "3.0.0"
info:
  title: test
  version: "1"
paths: {}
components:
  schemas:
    Address:
      type: object
      properties:
        street:
          type: string
          x-pii: true
        country:
          type: string
    Payment:
      type: object
      properties:
        amount:
          type: number
        creditorAddress:
          $ref: '#/components/schemas/Address'
"#;
        let api = parse_api(yaml);
        let paths = resolve_pii_paths(&api);
        assert!(paths.contains("creditorAddress.street"), "{paths:?}");
        assert!(!paths.contains("creditorAddress.country"), "{paths:?}");
        assert!(!paths.contains("amount"), "{paths:?}");
    }

    // ── array item PII ────────────────────────────────────────────────────────

    /// An array whose items have `x-pii: true` (e.g. an array of PII strings)
    /// must emit the array path with the `[]` suffix, not a plain property path.
    #[test]
    fn array_of_pii_strings_emits_array_path() {
        let yaml = r#"
openapi: "3.0.0"
info:
  title: test
  version: "1"
paths: {}
components:
  schemas:
    Addr:
      type: object
      properties:
        AddressLine:
          type: array
          items:
            type: string
            x-pii: true
        Country:
          type: string
"#;
        let api = parse_api(yaml);
        let paths = resolve_pii_paths(&api);
        assert!(paths.contains("AddressLine[]"), "{paths:?}");
        assert!(!paths.contains("Country"), "{paths:?}");
        // The bare property name without [] must not appear.
        assert!(!paths.contains("AddressLine"), "{paths:?}");
    }

    /// Array items that are a `$ref` to an object must expose PII fields from
    /// the referenced schema using `array[].field` notation.
    #[test]
    fn array_of_ref_objects_pii_resolved() {
        let yaml = r#"
openapi: "3.0.0"
info:
  title: test
  version: "1"
paths: {}
components:
  schemas:
    Account:
      type: object
      properties:
        Identification:
          type: string
          x-pii: true
        Currency:
          type: string
    Payment:
      type: object
      properties:
        accounts:
          type: array
          items:
            $ref: '#/components/schemas/Account'
"#;
        let api = parse_api(yaml);
        let paths = resolve_pii_paths(&api);
        assert!(paths.contains("accounts[].Identification"), "{paths:?}");
        assert!(!paths.contains("accounts[].Currency"), "{paths:?}");
    }

    // ── double-nested arrays ──────────────────────────────────────────────────

    /// Doubly-nested arrays (e.g. `Structured[].DocRefs[].Number`) must produce
    /// paths with two `[]` segments.
    #[test]
    fn double_nested_array_paths() {
        let yaml = r#"
openapi: "3.0.0"
info:
  title: test
  version: "1"
paths: {}
components:
  schemas:
    DocRef:
      type: object
      properties:
        Number:
          type: string
          x-pii: true
    Remittance:
      type: object
      properties:
        Structured:
          type: array
          items:
            type: object
            properties:
              ReferredDocumentInformation:
                type: array
                items:
                  $ref: '#/components/schemas/DocRef'
"#;
        let api = parse_api(yaml);
        let paths = resolve_pii_paths(&api);
        assert!(
            paths.contains("Structured[].ReferredDocumentInformation[].Number"),
            "{paths:?}"
        );
    }

    // ── iso-20022.yaml integration tests ──────────────────────────────────────

    const ISO_20022_YAML: &str = include_str!("../../../../config/iso-20022.yaml");

    /// The iso-20022.yaml file must parse without error using the openapiv3 crate.
    /// Failure here indicates an OpenAPI 3.1.0 / openapiv3 compatibility problem
    /// that must be resolved before the schema can be used in production.
    #[test]
    fn iso_20022_parses_successfully() {
        let api: OpenAPI =
            serde_yaml::from_str(ISO_20022_YAML).expect("iso-20022.yaml must parse as OpenAPI");
        assert!(
            api.components.is_some(),
            "iso-20022.yaml should have a components/schemas section"
        );
        let schema_count = api.components.as_ref().unwrap().schemas.len();
        assert!(
            schema_count > 0,
            "expected at least one component schema, got 0"
        );
    }

    /// Flat PII fields declared directly on a component schema (no `$ref` needed)
    /// must be detected. Validates against `OBCashAccount3`, which defines three
    /// known PII fields at its top level.
    #[test]
    fn iso_20022_flat_pii_fields_detected() {
        let api: OpenAPI = serde_yaml::from_str(ISO_20022_YAML).expect("iso-20022.yaml must parse");
        let paths = resolve_pii_paths(&api);

        // OBCashAccount3 declares Identification, Name, SecondaryIdentification as x-pii: true.
        for field in ["Identification", "Name", "SecondaryIdentification"] {
            assert!(
                paths.contains(field),
                "expected flat PII field '{field}' from OBCashAccount3 — got {paths:?}"
            );
        }
    }

    /// `OBPostalAddress6.AddressLine` is an array of strings where each item
    /// carries `x-pii: true`. The resolver must emit `AddressLine[]`.
    #[test]
    fn iso_20022_array_of_pii_strings_detected() {
        let api: OpenAPI = serde_yaml::from_str(ISO_20022_YAML).expect("iso-20022.yaml must parse");
        let paths = resolve_pii_paths(&api);
        assert!(
            paths.contains("AddressLine[]"),
            "expected 'AddressLine[]' (array of PII strings in OBPostalAddress6) — got {paths:?}"
        );
    }

    /// `OBDomesticPaymentInitiation4.DebtorAccount` is a `$ref` to `OBCashAccount3`.
    /// After `$ref` resolution the PII fields of `OBCashAccount3` must be reachable
    /// as `DebtorAccount.<field>`.
    #[test]
    fn iso_20022_ref_pii_fields_resolved() {
        let api: OpenAPI = serde_yaml::from_str(ISO_20022_YAML).expect("iso-20022.yaml must parse");
        let paths = resolve_pii_paths(&api);

        for field in ["Identification", "Name"] {
            let nested = format!("DebtorAccount.{field}");
            assert!(
                paths.contains(&nested),
                "expected '{nested}' via $ref resolution of DebtorAccount → OBCashAccount3 — got {paths:?}"
            );
        }
    }
}
