// SPDX-License-Identifier: MIT
// Copyright (c) 2026 James Maes
//! Notion database schema modeling and property resolution.
//!
//! The CLI accepts SQL column names while Notion requires canonical property
//! names and type-specific JSON keys. This module captures that mapping.
//!
//! # Responsibilities
//!
//! - Model the subset of Notion property types this CLI can translate
//!   ([`PropertyType`]).
//! - Hold the per-property schema of a single database column
//!   ([`PropertySchema`]).
//! - Parse a raw Notion database JSON response into a queryable schema and
//!   resolve user-supplied SQL column names back to Notion's canonical,
//!   case-exact property names ([`DatabaseSchema`]).
//!
//! # Why two name spaces
//!
//! SQL is conventionally case-insensitive for identifiers, but Notion property
//! names are case-sensitive and must be echoed back verbatim in API payloads.
//! To bridge the two, every property is stored under its exact Notion name
//! while a separate lowercase index (`DatabaseSchema::lookup`) lets callers
//! find it from any casing. The normalization rule is centralized in
//! `normalize_property_name` so both indexing and lookup stay consistent.
//!
//! # Fit within the crate
//!
//! Upstream the HTTP layer fetches the database JSON; this module turns that
//! JSON into a typed schema. Downstream the query/filter layer calls
//! [`DatabaseSchema::resolve_property`] to map SQL columns to Notion property
//! types and JSON keys when building filters and write payloads.

use std::collections::{BTreeMap, HashMap};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// Supported Notion database property types.
///
/// Each variant corresponds to a Notion property `type` string the CLI knows
/// how to filter on and coerce values for. Any type the CLI cannot yet handle
/// is preserved verbatim in [`PropertyType::Unsupported`] rather than dropped,
/// so error messages can name the offending Notion type and the original
/// string survives a round trip through [`PropertyType::notion_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyType {
    /// Notion title property.
    Title,
    /// Notion rich text property.
    RichText,
    /// Notion select property.
    Select,
    /// Notion status property.
    Status,
    /// Notion multi-select property.
    MultiSelect,
    /// Notion number property.
    Number,
    /// Notion checkbox property.
    Checkbox,
    /// Notion date property.
    Date,
    /// Any Notion property type that this CLI does not yet translate.
    Unsupported(String),
}

impl PropertyType {
    /// Converts Notion's `type` string into a local property type.
    ///
    /// `value` is the raw `type` field from a Notion property definition (for
    /// example `"rich_text"` or `"multi_select"`). Returns the matching
    /// variant, or [`PropertyType::Unsupported`] carrying the original string
    /// when the type is not one this CLI translates. This never fails: unknown
    /// types are captured, not rejected, so callers can report them later.
    pub fn from_notion_type(value: &str) -> Self {
        match value {
            "title" => Self::Title,
            "rich_text" => Self::RichText,
            "select" => Self::Select,
            "status" => Self::Status,
            "multi_select" => Self::MultiSelect,
            "number" => Self::Number,
            "checkbox" => Self::Checkbox,
            "date" => Self::Date,
            other => Self::Unsupported(other.to_string()),
        }
    }

    /// Returns the JSON key Notion expects for this property type.
    ///
    /// This is the key used inside a Notion property object (the same string as
    /// the `type` field). For [`PropertyType::Unsupported`] it returns the
    /// original captured string, making the round trip with
    /// [`PropertyType::from_notion_type`] lossless.
    pub fn notion_key(&self) -> &str {
        match self {
            Self::Title => "title",
            Self::RichText => "rich_text",
            Self::Select => "select",
            Self::Status => "status",
            Self::MultiSelect => "multi_select",
            Self::Number => "number",
            Self::Checkbox => "checkbox",
            Self::Date => "date",
            Self::Unsupported(value) => value,
        }
    }

    /// Reports whether the property type can be used for filters and writes.
    ///
    /// Returns `true` for every concrete variant and `false` only for
    /// [`PropertyType::Unsupported`]. Used as the gate in
    /// [`DatabaseSchema::resolve_property`] so a known-but-untranslatable column
    /// is rejected with a clear error instead of producing an invalid payload.
    pub fn is_supported(&self) -> bool {
        // Every variant except `Unsupported` is something we can translate.
        !matches!(self, Self::Unsupported(_))
    }
}

/// Schema for one Notion database property.
///
/// Pairs a property's canonical (case-exact) Notion name with its translated
/// type. The `name` is what must be sent back to Notion verbatim; the
/// `property_type` drives how values are filtered and coerced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertySchema {
    /// Canonical Notion property name.
    pub name: String,
    /// Notion property type used for filters and value coercion.
    pub property_type: PropertyType,
}

/// Parsed schema for a Notion database.
///
/// Holds two views of the same set of properties:
///
/// - `properties`, the source of truth, keyed by exact Notion name. A
///   [`BTreeMap`] is used so iteration and the `available_columns` /
///   `unsupported_columns` listings come out in a stable, sorted order.
/// - `lookup`, a fast case-insensitive index for resolving SQL column names.
///
/// # Invariants
///
/// - Every value in `lookup` is a key present in `properties`.
/// - Normalized keys are unique: construction (see
///   [`DatabaseSchema::from_notion_database`]) rejects databases where two
///   property names collide after normalization, so `lookup` can never silently
///   shadow one property with another.
#[derive(Debug, Clone)]
pub struct DatabaseSchema {
    /// Canonical property schemas keyed by exact Notion property name.
    properties: BTreeMap<String, PropertySchema>,
    /// Lowercase lookup from user-provided SQL column names to canonical names.
    lookup: HashMap<String, String>,
}

impl DatabaseSchema {
    /// Parses the schema section of a Notion database response.
    ///
    /// `database` is the JSON object returned by Notion's "retrieve a database"
    /// endpoint. Only its `properties` object is read; each entry's key is the
    /// canonical property name and its `type` field selects the
    /// [`PropertyType`]. Returns a fully indexed [`DatabaseSchema`] on success.
    ///
    /// # Errors
    ///
    /// Returns an error when:
    /// - the response has no `properties` object;
    /// - two or more property names collide after case-insensitive
    ///   normalization, which would make SQL column lookup ambiguous; or
    /// - any property is missing its `type` string.
    pub fn from_notion_database(database: &Value) -> Result<Self> {
        let properties = database
            .get("properties")
            .and_then(Value::as_object)
            .context("Notion database response did not include a properties object")?;

        // First pass: group the exact names by their normalized form so we can
        // detect case-insensitive collisions before building any index.
        let mut normalized_names: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in properties.keys() {
            normalized_names
                .entry(normalize_property_name(name))
                .or_default()
                .push(name.clone());
        }

        // Any normalized key mapping to more than one original name is
        // ambiguous: a SQL lookup could not decide which property was meant.
        // Sort the colliding names so the error message is deterministic.
        let ambiguities = normalized_names
            .into_iter()
            .filter_map(|(normalized, mut names)| {
                if names.len() <= 1 {
                    return None;
                }
                names.sort();
                Some(format!("{normalized} ({})", names.join(", ")))
            })
            .collect::<Vec<_>>();
        if !ambiguities.is_empty() {
            bail!(
                "Ambiguous Notion property names for case-insensitive SQL lookup: {}",
                ambiguities.join("; ")
            );
        }

        // Second pass: build both views. Collisions are already ruled out, so
        // each `lookup` insert maps to a distinct canonical name.
        let mut parsed = BTreeMap::new();
        let mut lookup = HashMap::new();
        for (name, property) in properties {
            // Keep a canonical name and a normalized lookup so SQL remains
            // forgiving while outgoing Notion payloads preserve exact casing.
            let type_name = property
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Notion property '{name}' did not include a type"))?;
            let schema = PropertySchema {
                name: name.clone(),
                property_type: PropertyType::from_notion_type(type_name),
            };
            lookup.insert(normalize_property_name(name), name.clone());
            parsed.insert(name.clone(), schema);
        }

        Ok(Self {
            properties: parsed,
            lookup,
        })
    }

    /// Resolves a SQL column name to a supported Notion property schema.
    ///
    /// `requested` is a user-supplied column name in any casing. It is
    /// normalized and looked up against the case-insensitive index; on a match
    /// the canonical [`PropertySchema`] is returned by reference.
    ///
    /// # Errors
    ///
    /// Returns an error when:
    /// - no property matches `requested` (the message lists the available
    ///   columns to aid the user);
    /// - the lookup points at a canonical name absent from `properties`, which
    ///   would mean the type's invariant was violated; or
    /// - the matched property has an unsupported Notion type and therefore
    ///   cannot be used in a filter or write.
    pub fn resolve_property(&self, requested: &str) -> Result<&PropertySchema> {
        let normalized = normalize_property_name(requested);
        let canonical = self.lookup.get(&normalized).ok_or_else(|| {
            anyhow!(
                "Column '{requested}' does not exist. Available columns: {}",
                self.available_columns().join(", ")
            )
        })?;

        // Indirection through `lookup` then `properties` should always succeed
        // given the type's invariants; a miss signals internal corruption
        // rather than bad user input, so it gets a distinct message.
        let property = self
            .properties
            .get(canonical)
            .ok_or_else(|| anyhow!("Internal schema lookup failed for '{canonical}'"))?;

        if !property.property_type.is_supported() {
            bail!(
                "Column '{}' has unsupported Notion property type '{}'",
                property.name,
                property.property_type.notion_key()
            );
        }

        Ok(property)
    }

    /// Returns canonical property names in stable display order.
    ///
    /// Order is the [`BTreeMap`] key ordering (lexicographic by exact name), so
    /// repeated calls and the error messages built from them are deterministic.
    pub fn available_columns(&self) -> Vec<String> {
        self.properties.keys().cloned().collect()
    }

    /// Returns unsupported property names and their Notion type keys in stable order.
    ///
    /// Each entry is formatted as `name (type_key)` for the properties whose
    /// type the CLI cannot translate. Intended for surfacing skipped columns to
    /// the user, e.g. in a schema-inspection command.
    pub fn unsupported_columns(&self) -> Vec<String> {
        self.properties
            .values()
            .filter(|property| !property.property_type.is_supported())
            .map(|property| {
                format!(
                    "{} ({})",
                    property.name,
                    property.property_type.notion_key()
                )
            })
            .collect()
    }

    /// Iterates over all property schemas in stable display order.
    ///
    /// Yields every property, supported or not, in [`BTreeMap`] key order.
    /// Callers that only want usable columns should filter on
    /// [`PropertyType::is_supported`].
    pub fn properties(&self) -> impl Iterator<Item = &PropertySchema> {
        self.properties.values()
    }
}

/// Normalizes property names for case-insensitive SQL column lookup.
///
/// `value` is any property or column name; the return value is its
/// lowercased form. ASCII-only lowercasing is deliberate: it is locale-stable
/// (no Turkish-`i` style surprises) and matches Notion's effectively ASCII
/// property naming, keeping indexing in [`DatabaseSchema::from_notion_database`]
/// and lookup in [`DatabaseSchema::resolve_property`] in lockstep. This is the
/// single definition of normalization; both call sites must route through it.
fn normalize_property_name(value: &str) -> String {
    value.to_ascii_lowercase()
}
