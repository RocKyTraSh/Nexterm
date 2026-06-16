use zeroize::{Zeroize, ZeroizeOnDrop};

/// A secret string (password, passphrase, token) that is zeroed on drop and
/// never prints its contents via `Debug`/`Display`.
///
/// There is intentionally **no** `Display` and **no** `Serialize`: secrets must
/// not flow into logs, UIs, or config files by accident.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secret {
    value: String,
}

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    /// Borrow the secret. Callers must not log or persist the result.
    pub fn expose(&self) -> &str {
        &self.value
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}
