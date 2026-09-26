use std::{error::Error, fmt};

use super::{
    MAX_PAGE_SIZE, PageLimit, PageLimitError, RETAINED_EVENT_CURSOR_PREFIX, RetainedEventCursor,
    StorageError, StorageErrorCode,
};

impl RetainedEventCursor {
    /// Creates a namespaced durable event cursor.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the cursor is empty, belongs to another sequence namespace,
    /// or has no opaque storage position.
    pub fn new(value: impl Into<String>) -> Result<Self, StorageError> {
        let value = value.into();
        let Some(position) = value.strip_prefix(RETAINED_EVENT_CURSOR_PREFIX) else {
            return Err(StorageError::new(
                StorageErrorCode::InvalidRequest,
                "durable event cursor has the wrong namespace",
            ));
        };
        if position.trim().is_empty() {
            return Err(StorageError::new(
                StorageErrorCode::InvalidRequest,
                "durable event cursor position cannot be empty",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the stable opaque representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PageLimit {
    /// Creates a page limit between one and [`MAX_PAGE_SIZE`], inclusive.
    ///
    /// # Errors
    ///
    /// Returns [`PageLimitError`] for zero or an oversized request.
    pub const fn new(value: u16) -> Result<Self, PageLimitError> {
        if value == 0 {
            Err(PageLimitError::Zero)
        } else if value > MAX_PAGE_SIZE {
            Err(PageLimitError::TooLarge {
                requested: value,
                maximum: MAX_PAGE_SIZE,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the validated bound.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for PageLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("page limit must be greater than zero"),
            Self::TooLarge { requested, maximum } => {
                write!(
                    formatter,
                    "page limit {requested} exceeds maximum {maximum}"
                )
            }
        }
    }
}

impl Error for PageLimitError {}
