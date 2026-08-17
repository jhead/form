//! Port of `packages/ai/src/auth/oauth/pkce.ts` (RFC 7636).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// 32 random bytes as the base64url verifier, S256 challenge over it.
pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = pkce_challenge(&verifier);
    Pkce {
        verifier,
        challenge,
    }
}

/// `BASE64URL(SHA256(ASCII(verifier)))`.
pub fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_and_challenge_are_base64url_without_padding() {
        let pkce = generate_pkce();
        assert_eq!(pkce.verifier.len(), 43);
        assert_eq!(pkce.challenge.len(), 43);
        for value in [&pkce.verifier, &pkce.challenge] {
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "unexpected character in {value}"
            );
        }
    }

    #[test]
    fn challenge_matches_the_rfc_7636_appendix_b_vector() {
        // RFC 7636 B.1/B.2: verifier -> S256 challenge.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifiers_are_unique() {
        assert_ne!(generate_pkce().verifier, generate_pkce().verifier);
    }
}
