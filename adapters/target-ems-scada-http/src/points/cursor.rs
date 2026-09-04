use crate::error::IntegrationErrorCode;

/// Namespace required for bounded point-page cursors.
///
/// The prefix keeps a station-page cursor from being accepted as a point-page position, and the
/// reverse, so a client cannot silently resume one bounded list from another's checkpoint.
pub(crate) const POINT_CURSOR_PREFIX: &str = "uob:point:";

/// Resume position inside the flattened canonical point list.
///
/// A point page is derived from one bounded station-snapshot read, so a position needs both the
/// station page it continues and how many of that page's points were already delivered.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PointCursor {
    /// Points already delivered from the station page named by `after`.
    pub(crate) delivered: usize,
    /// Station-page cursor whose snapshots this position refers to.
    pub(crate) after: Option<String>,
}

impl PointCursor {
    /// Builds the position that continues the current station page.
    pub(crate) fn resume(delivered: usize, after: Option<String>) -> Self {
        Self { delivered, after }
    }

    /// Builds the position that starts the next station page.
    pub(crate) fn advance(after: String) -> Self {
        Self {
            delivered: 0,
            after: Some(after),
        }
    }

    /// Renders the opaque wire cursor.
    pub(crate) fn encode(&self) -> String {
        format!(
            "{POINT_CURSOR_PREFIX}{}:{}",
            self.delivered,
            self.after.as_deref().unwrap_or_default()
        )
    }

    /// Parses an opaque wire cursor.
    ///
    /// The station-page position is kept last and unescaped: it is the host's own opaque text and
    /// may contain any visible character, including the separator.
    pub(crate) fn decode(value: &str) -> Result<Self, IntegrationErrorCode> {
        let position = value
            .strip_prefix(POINT_CURSOR_PREFIX)
            .ok_or(IntegrationErrorCode::InvalidRequest)?;
        let (delivered, after) = position
            .split_once(':')
            .ok_or(IntegrationErrorCode::InvalidRequest)?;
        let delivered = delivered
            .parse::<usize>()
            .map_err(|_| IntegrationErrorCode::InvalidRequest)?;
        Ok(Self {
            delivered,
            after: (!after.is_empty()).then(|| after.to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{IntegrationErrorCode, PointCursor};

    #[test]
    fn a_position_survives_one_encode_and_decode_round_trip() {
        for cursor in [
            PointCursor::default(),
            PointCursor::resume(7, None),
            PointCursor::advance("snapshot:page:2".to_owned()),
            // The host's own cursor text may contain the separator.
            PointCursor::resume(3, Some("snapshot:page:2".to_owned())),
        ] {
            assert_eq!(PointCursor::decode(&cursor.encode()), Ok(cursor));
        }
    }

    #[test]
    fn a_cursor_from_another_bounded_list_is_refused() {
        for value in [
            "",
            "uob:event:0:x",
            "snapshot:next",
            "uob:point:",
            "uob:point:x:y",
        ] {
            assert_eq!(
                PointCursor::decode(value),
                Err(IntegrationErrorCode::InvalidRequest),
                "{value}"
            );
        }
    }
}
