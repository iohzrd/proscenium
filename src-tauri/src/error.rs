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
