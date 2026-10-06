use serde::{
    Deserializer as _,
    de::{IgnoredAny, SeqAccess, Visitor},
};
use serde_json::value::RawValue;
use std::{borrow::Cow, fmt};

pub(super) struct Header<'a> {
    pub report: bool,
    pub response_id: Option<Cow<'a, str>>,
}
fn text(value: Option<&RawValue>) -> Option<Cow<'_, str>> {
    let raw = value?.get();
    if raw.len() > 2048 {
        return None;
    }
    if !raw.as_bytes().contains(&b'\\') {
        return raw
            .strip_prefix('"')
            .and_then(|raw| raw.strip_suffix('"'))
            .map(Cow::Borrowed);
    }
    serde_json::from_str::<String>(raw).ok().map(Cow::Owned)
}
struct Probe;
impl<'de> Visitor<'de> for Probe {
    type Value = Header<'de>;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OCPP frame array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let kind = sequence.next_element::<&RawValue>()?;
        let id = sequence.next_element::<&RawValue>()?;
        let third = sequence.next_element::<&RawValue>()?;
        let report = kind.is_some_and(|kind| kind.get() == "2")
            && text(third)
                .as_deref()
                .and_then(super::reports::ReportKind::from_action)
                .is_some();
        let response_id = if kind.is_some_and(|kind| matches!(kind.get(), "3" | "4")) {
            text(id)
        } else {
            None
        };
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Header {
            report,
            response_id,
        })
    }
}
pub(super) fn parse(bytes: &[u8]) -> Option<Header<'_>> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let header = decoder.deserialize_seq(Probe).ok()?;
    decoder.end().ok()?;
    Some(header)
}
