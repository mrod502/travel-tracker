//! How the byte fields of a CA document look once it is JSON.
//!
//! A credential and a revocation status list are both stored as JSON somewhere —
//! a credential as the bytes in `nodes.ca_credential`, a published list as
//! `revocation_status_lists.rsl` — and both carry keys, signatures and
//! identifiers. Left to serde's default, a `Vec<u8>` becomes an array of 32 or 64
//! integers: technically lossless, but not a representation any other tool
//! recognises, unreadable to anyone who has to compare two documents by eye, and
//! four to eight times the size it needs to be.
//!
//! Base64 is what the rest of the ecosystem writes in this position (a JWK's
//! `x`, a COSE key's `x`/`y`, an ACME JWS signature), so this module swaps the
//! array spelling for that one, on the fields that hold key material rather than
//! on the document as a whole.
//!
//! Raw bytes remain the representation *everywhere else*: the values these
//! functions hand back are `Vec<u8>`, the columns they land in are `BYTEA`, and
//! the bytes a signature covers are the bytes themselves, not this encoding of
//! them. Base64 exists only inside a JSON document, because JSON has no byte type.

/// `&[u8]` ↔ base64 string, for required byte fields.
pub mod base64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

/// `Option<Vec<u8>>` ↔ base64 string or absent, for optional byte fields.
pub mod optional_base64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        bytes: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => serializer.serialize_str(&STANDARD.encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        let text = Option::<String>::deserialize(deserializer)?;
        text.map(|text| STANDARD.decode(text).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Doc {
        #[serde(with = "base64_bytes")]
        required: Vec<u8>,
        #[serde(
            with = "optional_base64_bytes",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        optional: Option<Vec<u8>>,
    }

    #[test]
    fn byte_fields_are_base64_strings_not_arrays_of_integers() {
        let doc = Doc {
            required: (0u8..32).collect(),
            optional: Some(vec![0xff, 0xfe, 0xfd]),
        };

        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(
            json["required"],
            serde_json::json!("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=")
        );
        assert_eq!(json["optional"], serde_json::json!("//79"));
    }

    #[test]
    fn a_document_round_trips_through_its_own_json() {
        let doc = Doc {
            required: vec![0u8; 64],
            optional: None,
        };

        let encoded = serde_json::to_string(&doc).unwrap();
        // Absent rather than `null`, so the stored document carries no key at all
        // for a field that has no value.
        assert!(!encoded.contains("optional"));
        assert_eq!(serde_json::from_str::<Doc>(&encoded).unwrap(), doc);
    }

    #[test]
    fn bytes_that_are_not_base64_are_refused_not_guessed_at() {
        // The old spelling of these fields was an array of integers; reading it
        // back as a string is what fails here, which is the point — a document
        // written by the old code is not silently mis-read as a new one.
        let err = serde_json::from_str::<Doc>(r#"{"required":[1,2,3]}"#).unwrap_err();
        assert!(err.to_string().contains("invalid type"), "{err}");
    }
}
