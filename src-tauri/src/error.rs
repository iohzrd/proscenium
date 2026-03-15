/// Unified error type for the entire application.
///
/// Every layer (storage, service, command) returns `Result<T, AppError>`.
/// At the Tauri command boundary the manual `Serialize` impl converts it
/// to a plain string for the frontend.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // -- storage / database --
    #[error("{0}")]
    Sqlx(#[from] sqlx::Error),

    // -- serialization --
    #[error("{0}")]
    SerdeJson(#[from] serde_json::Error),

    // -- HTTP --
    #[error("{0}")]
    Reqwest(#[from] reqwest::Error),

    // -- crypto --
    #[error("{0}")]
    Snow(#[from] snow::Error),

    #[error("{0}")]
    Crypto(#[from] crate::crypto::CryptoError),

    // -- encoding --
    #[error("{0}")]
    HexDecode(#[from] hex::FromHexError),

    #[error("{0}")]
    Base64Decode(#[from] base64::DecodeError),

    // -- key generation --
    #[error("{0}")]
    Bip39(#[from] bip39::Error),

    #[error("{0}")]
    Getrandom(#[from] getrandom::Error),

    // -- IO --
    #[error("{0}")]
    Io(#[from] std::io::Error),

    // -- string conversion --
    #[error("{0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    // -- Tauri --
    #[error("{0}")]
    Tauri(#[from] tauri::Error),

    // -- iroh networking --
    #[error("{0}")]
    Connect(#[from] iroh::endpoint::ConnectError),

    #[error("{0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),

    #[error("{0}")]
    Write(#[from] iroh::endpoint::WriteError),

    #[error("{0}")]
    ReadExact(#[from] iroh::endpoint::ReadExactError),

    #[error("{0}")]
    ReadToEnd(#[from] iroh::endpoint::ReadToEndError),

    #[error("{0}")]
    ClosedStream(#[from] iroh::endpoint::ClosedStream),

    #[error("{0}")]
    KeyParsing(#[from] iroh::KeyParsingError),

    // -- iroh gossip --
    #[error("{0}")]
    GossipApi(#[from] iroh_gossip::api::ApiError),

    // -- iroh blobs --
    #[error("{0}")]
    BlobRequest(#[from] iroh_blobs::api::RequestError),

    #[error("{0}")]
    BlobGet(#[from] iroh_blobs::get::GetError),

    #[error("{0}")]
    BlobExportBao(#[from] iroh_blobs::api::ExportBaoError),

    // -- ad-hoc / string errors --
    #[error("{0}")]
    Other(String),
}

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError::Other(s)
    }
}

impl From<&str> for AppError {
    fn from(s: &str) -> Self {
        AppError::Other(s.to_owned())
    }
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Shorthand used by all command return types.
pub type CmdResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_from_string() {
        let err = AppError::from("something went wrong".to_string());
        assert_eq!(err.to_string(), "something went wrong");
    }

    #[test]
    fn error_from_str() {
        let err = AppError::from("bad input");
        assert_eq!(err.to_string(), "bad input");
    }

    #[test]
    fn error_serializes_to_string() {
        let err = AppError::Other("test error".into());
        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(json, r#""test error""#);
    }

    #[test]
    fn error_display_shows_inner() {
        let err = AppError::HexDecode(hex::FromHexError::OddLength);
        let display = err.to_string();
        assert!(!display.is_empty());
    }

    #[test]
    fn error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err = AppError::from(io_err);
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn error_from_serde_json() {
        let bad_json = "not json at all {{{";
        let result: Result<serde_json::Value, _> = serde_json::from_str(bad_json);
        let err = AppError::from(result.unwrap_err());
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn error_from_base64() {
        use base64::Engine;
        let result = base64::engine::general_purpose::STANDARD.decode("!!!invalid!!!");
        let err = AppError::from(result.unwrap_err());
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn error_from_hex_decode() {
        let result = hex::decode("zzzz");
        let err = AppError::from(result.unwrap_err());
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn error_from_utf8() {
        let bad_bytes = vec![0xff, 0xfe];
        let result = String::from_utf8(bad_bytes);
        let err = AppError::from(result.unwrap_err());
        assert!(!err.to_string().is_empty());
    }
}
