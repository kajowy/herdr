use std::io;

pub(crate) mod destination;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransferError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl TransferError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[allow(dead_code)] // wired to the upload request handler in a later task
    pub(crate) fn from_io(code: &'static str, err: &io::Error) -> Self {
        Self::new(code, err.to_string())
    }
}
