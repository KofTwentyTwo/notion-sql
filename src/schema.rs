//! Notion database schema modeling and property resolution.
//!
//! The CLI accepts SQL column names while Notion requires canonical property
//! names and type-specific JSON keys. This module captures that mapping.

use std::collections::{BTreeMap, HashMap};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// Supported Notion database property types.
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
    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::Unsupported(_))
    }
}

/// Schema for one Notion database property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertySchema {
    /// Canonical Notion property name.
    pub name: String,
    /// Notion property type used for filters and value coercion.
    pub property_type: PropertyType,
}

/// Parsed schema for a Notion database.
#[derive(Debug, Clone)]
pub struct DatabaseSchema {
    /// Canonical property schemas keyed by exact Notion property name.
    properties: BTreeMap<String, PropertySchema>,
    /// Lowercase lookup from user-provided SQL column names to canonical names.
    lookup: HashMap<String, String>,
}

impl DatabaseSchema {
    /// Parses the schema section of a Notion database response.
    pub fn from_notion_database(database: &Value) -> Result<Self> {
        let properties = database
            .get("properties")
            .and_then(Value::as_object)
            .context("Notion database response did not include a properties object")?;

        let mut normalized_names: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in properties.keys() {
            normalized_names
                .entry(normalize_property_name(name))
                .or_default()
                .push(name.clone());
        }

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
    pub fn resolve_property(&self, requested: &str) -> Result<&PropertySchema> {
        let normalized = normalize_property_name(requested);
        let canonical = self.lookup.get(&normalized).ok_or_else(|| {
            anyhow!(
                "Column '{requested}' does not exist. Available columns: {}",
                self.available_columns().join(", ")
            )
        })?;

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
    pub fn available_columns(&self) -> Vec<String> {
        self.properties.keys().cloned().collect()
    }

    /// Returns unsupported property names and their Notion type keys in stable order.
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
    pub fn properties(&self) -> impl Iterator<Item = &PropertySchema> {
        self.properties.values()
    }
}

/// Normalizes property names for case-insensitive SQL column lookup.
fn normalize_property_name(value: &str) -> String {
    value.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    //! Tests for Notion schema parsing and property lookup safety.

    use serde_json::json;

    use super::*;

    /// Verifies that case-insensitive lookup ambiguities fail during schema parsing.
    #[test]
    fn rejects_case_insensitive_property_name_collisions() {
        let error = DatabaseSchema::from_notion_database(&json!({
            "properties": {
                "Status": { "type": "status", "status": {} },
                "status": { "type": "select", "select": {} }
            }
        }))
        .unwrap_err()
        .to_string();

        assert!(error.contains("Ambiguous Notion property names"));
        assert!(error.contains("Status"));
        assert!(error.contains("status"));
    }

    /// Verifies unsupported columns are listed for wildcard projection validation.
    #[test]
    fn lists_unsupported_columns() {
        let schema = DatabaseSchema::from_notion_database(&json!({
            "properties": {
                "Name": { "type": "title", "title": {} },
                "Formula": { "type": "formula", "formula": {} }
            }
        }))
        .unwrap();

        assert_eq!(
            schema.unsupported_columns(),
            vec!["Formula (formula)".to_string()]
        );
    }
}
