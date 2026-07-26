use tempo_agentic_domain::ChainFamily;
use thiserror::Error;

use crate::secret_file::SecretFileError;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("invalid {family} private key: {reason}")]
    KeyLoad { family: ChainFamily, reason: String },
    #[error("signing failed: {0}")]
    Sign(String),
    #[error("no {0} key is configured")]
    NoKey(ChainFamily),
    #[error("the {family} key cannot sign a transaction built for another chain")]
    WrongFamily { family: ChainFamily },
    #[error(transparent)]
    File(#[from] SecretFileError),
}
