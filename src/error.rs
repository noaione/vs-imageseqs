use std::{
    error::Error,
    ffi::{CStr, CString},
    fmt::{self, Display},
};

/// Error type passed back to VapourSynth.
#[derive(Debug)]
pub struct ImgSeqError {
    message: CString,
}

impl ImgSeqError {
    pub fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        let message = CString::new(message)
            .unwrap_or_else(|_| CString::new("image sequence error contained a NUL byte").unwrap());
        Self { message }
    }

    pub fn from_display(error: impl Display) -> Self {
        Self::new(error.to_string())
    }
}

impl AsRef<CStr> for ImgSeqError {
    fn as_ref(&self) -> &CStr {
        &self.message
    }
}

impl Display for ImgSeqError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.to_string_lossy().fmt(formatter)
    }
}

impl Error for ImgSeqError {}

pub type Result<T> = std::result::Result<T, ImgSeqError>;
