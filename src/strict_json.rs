// Copyright © 2026 Manuel Koch
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! JSON object parsing that rejects duplicate keys, including nested keys.

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt;

pub(crate) enum ObjectError {
    Malformed,
    DuplicateKeys,
}

/// Read a JSON object in source order without silently merging duplicate keys,
/// including inside nested objects.
pub(crate) fn object(bytes: &[u8]) -> Result<Map<String, Value>, ObjectError> {
    let StrictValue(value, duplicate) =
        serde_json::from_slice(bytes).map_err(|_| ObjectError::Malformed)?;
    match value {
        Value::Object(_) if duplicate => Err(ObjectError::DuplicateKeys),
        Value::Object(fields) => Ok(fields),
        _ => Err(ObjectError::Malformed),
    }
}

struct StrictValue(Value, bool);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut fields = Map::new();
        let mut duplicate = false;
        while let Some((key, StrictValue(value, nested_duplicate))) = access.next_entry()? {
            duplicate |= fields.insert(key, value).is_some() || nested_duplicate;
        }
        Ok(StrictValue(Value::Object(fields), duplicate))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut elements = Vec::new();
        let mut duplicate = false;
        while let Some(StrictValue(value, nested_duplicate)) = access.next_element()? {
            elements.push(value);
            duplicate |= nested_duplicate;
        }
        Ok(StrictValue(Value::Array(elements), duplicate))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::from(value), false))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::from(value), false))
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::from(value), false))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::from(value), false))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::from(value), false))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::from(value), false))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null, false))
    }
}
