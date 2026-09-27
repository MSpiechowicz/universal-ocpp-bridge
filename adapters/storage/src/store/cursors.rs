use uob_application::{
    RETAINED_EVENT_CURSOR_PREFIX, RetainedEventCursor, StorageError, StorageErrorCode,
};

pub(super) fn event_cursor(
    value: Option<&RetainedEventCursor>,
) -> Result<Option<i64>, StorageError> {
    value
        .map(|value| {
            value
                .as_str()
                .strip_prefix(RETAINED_EVENT_CURSOR_PREFIX)
                .and_then(|position| position.parse::<i64>().ok())
                .filter(|position| *position > 0)
                .ok_or_else(|| {
                    StorageError::new(
                        StorageErrorCode::CursorExpired,
                        "durable event cursor expired; fetch a fresh snapshot",
                    )
                })
        })
        .transpose()
}
