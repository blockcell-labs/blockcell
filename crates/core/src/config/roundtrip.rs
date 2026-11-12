//! Preserve extensions and environment references without reviving typed deletions.
use std::cell::RefCell;
use std::collections::HashMap;

use serde::de::{DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::{Config, Result};

type Fields = HashMap<Vec<String>, &'static [&'static str]>;

/// The deserializer records declared fields, including fields omitted by serde's
/// skip_serializing_if. Comparing serialized values alone would misclassify those
/// fields as extensions and restore old defaults after an explicit reset.
struct SchemaDeserializer<'a> {
    value: &'a Value,
    path: Vec<String>,
    fields: &'a RefCell<Fields>,
}

impl<'de> Deserializer<'de> for SchemaDeserializer<'de> {
    type Error = serde_json::Error;

    fn deserialize_any<V: Visitor<'de>>(
        self,
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        match self.value {
            Value::Null => visitor.visit_unit(),
            Value::Bool(value) => visitor.visit_bool(*value),
            Value::Number(_) => self.value.deserialize_any(visitor),
            Value::String(value) => visitor.visit_borrowed_str(value),
            Value::Array(values) => visitor.visit_seq(SchemaSeq {
                values: values.iter().enumerate(),
                path: self.path,
                fields: self.fields,
            }),
            Value::Object(values) => visitor.visit_map(SchemaMap {
                values: values.iter(),
                pending: None,
                path: self.path,
                fields: self.fields,
            }),
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        self.fields.borrow_mut().insert(self.path.clone(), fields);
        self.deserialize_any(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(
        self,
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        if self.value.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        self.value.deserialize_enum(name, variants, visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(
        self,
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes
        byte_buf unit unit_struct seq tuple tuple_struct map identifier
    }
}

struct SchemaSeq<'a> {
    values: std::iter::Enumerate<std::slice::Iter<'a, Value>>,
    path: Vec<String>,
    fields: &'a RefCell<Fields>,
}

impl<'de> SeqAccess<'de> for SchemaSeq<'de> {
    type Error = serde_json::Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> std::result::Result<Option<T::Value>, Self::Error> {
        let Some((index, value)) = self.values.next() else {
            return Ok(None);
        };
        let mut path = self.path.clone();
        path.push(index.to_string());
        seed.deserialize(SchemaDeserializer {
            value,
            path,
            fields: self.fields,
        })
        .map(Some)
    }
}

struct SchemaMap<'a> {
    values: serde_json::map::Iter<'a>,
    pending: Option<(&'a String, &'a Value)>,
    path: Vec<String>,
    fields: &'a RefCell<Fields>,
}

impl<'de> MapAccess<'de> for SchemaMap<'de> {
    type Error = serde_json::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> std::result::Result<Option<K::Value>, Self::Error> {
        let Some((key, value)) = self.values.next() else {
            return Ok(None);
        };
        self.pending = Some((key, value));
        seed.deserialize(key.as_str().into_deserializer()).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        let (key, value) = self.pending.take().expect("value follows map key");
        let mut path = self.path.clone();
        path.push(key.clone());
        seed.deserialize(SchemaDeserializer {
            value,
            path,
            fields: self.fields,
        })
    }
}

#[derive(PartialEq, Eq)]
pub(super) enum Identity<'a> {
    Id(&'a str),
    Model(&'a str, &'a str),
}

pub(super) fn identity(value: &Value) -> Option<Identity<'_>> {
    if let Some(id) = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        Some(Identity::Id(id))
    } else {
        Some(Identity::Model(
            value.get("provider")?.as_str()?,
            value.get("model")?.as_str()?,
        ))
    }
}

fn is_known(
    known: &serde_json::Map<String, Value>,
    fields: &Fields,
    path: &[String],
    key: &str,
) -> bool {
    known.contains_key(key)
        || fields
            .get(path)
            .is_some_and(|declared| declared.contains(&key))
}

fn needs_preservation(raw: &Value, known: &Value, fields: &Fields, path: &mut Vec<String>) -> bool {
    match (raw, known) {
        (Value::Object(raw), Value::Object(known)) => raw.iter().any(|(key, value)| {
            if !is_known(known, fields, path, key) {
                return true;
            }
            let Some(known) = known.get(key) else {
                return false;
            };
            path.push(key.clone());
            let result = needs_preservation(value, known, fields, path);
            path.pop();
            result
        }),
        (Value::Array(raw), Value::Array(known)) => {
            raw.iter()
                .zip(known)
                .enumerate()
                .any(|(index, (raw, known))| {
                    path.push(index.to_string());
                    let result = needs_preservation(raw, known, fields, path);
                    path.pop();
                    result
                })
        }
        (Value::String(value), _) => value.contains("${"),
        _ => false,
    }
}

fn merge(
    raw: &Value,
    known: &Value,
    next: &mut Value,
    fields: &Fields,
    path: &mut Vec<String>,
) -> Result<()> {
    match (raw, known, next) {
        (Value::Object(raw), Value::Object(known), Value::Object(next)) => {
            for (key, value) in raw {
                if !is_known(known, fields, path, key) {
                    next.entry(key.clone()).or_insert_with(|| value.clone());
                } else if let (Some(known), Some(next)) = (known.get(key), next.get_mut(key)) {
                    path.push(key.clone());
                    merge(value, known, next, fields, path)?;
                    path.pop();
                }
            }
        }
        (Value::Array(raw), Value::Array(known), Value::Array(next)) => {
            for (index, (raw, item)) in raw.iter().zip(known).enumerate() {
                path.push(index.to_string());
                if needs_preservation(raw, item, fields, path) {
                    let candidates: Vec<usize> = next
                        .iter()
                        .enumerate()
                        .filter_map(|(index, next)| match identity(item) {
                            Some(id) => (identity(next) == Some(id)).then_some(index),
                            None => (item == next).then_some(index),
                        })
                        .collect();
                    let old_count = known
                        .iter()
                        .filter(|old| match identity(item) {
                            Some(id) => identity(old) == Some(id),
                            None => *old == item,
                        })
                        .count();
                    if candidates.len() > 1 || (!candidates.is_empty() && old_count != 1) {
                        return Err(crate::Error::Config(format!("Cannot identify array entry at {} to preserve configuration extensions", path.join("."))));
                    }
                    if let Some(index) = candidates.first() {
                        merge(raw, item, &mut next[*index], fields, path)?;
                    } else if identity(item).is_none() && !next.is_empty() {
                        return Err(crate::Error::Config(format!("Array entry at {} needs a stable id to preserve configuration extensions", path.join("."))));
                    }
                }
                path.pop();
            }
        }
        (Value::String(raw), known, next) if raw.contains("${") && known == next => {
            *next = Value::String(raw.clone());
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn preserve(raw: &Value, expanded: &Value, next: &mut Value) -> Result<()> {
    let fields = RefCell::new(Fields::new());
    let previous = Config::deserialize(SchemaDeserializer {
        value: expanded,
        path: Vec::new(),
        fields: &fields,
    })?;
    let known = serde_json::to_value(previous)?;
    merge(raw, &known, next, &fields.into_inner(), &mut Vec::new())
}
