//! Authentication: password hashing and signed session tokens.
//!
//! Two deliberate changes from the Python version:
//!
//! * The hash format carries its parameters (`pbkdf2$sha256$iters$salt$hash`),
//!   so the iteration count can be raised later without invalidating stored
//!   passwords. The Python format was `salt:hash` with the parameters hardcoded
//!   at the verification site.
//! * Comparison is constant time via `subtle`, and the session signature is
//!   verified before the payload is parsed, so a forged token is rejected
//!   without touching the timestamp.

use base64::Engine;
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

const ITERATIONS: u32 = 210_000;
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;
pub const COOKIE_NAME: &str = "mindash_session";
/// Sessions are valid for 30 days, matching the previous behaviour.
const SESSION_TTL_SECS: u64 = 30 * 24 * 3600;

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()
}

/// Derive a storable hash: `pbkdf2$sha256$<iters>$<salt_b64>$<hash_b64>`.
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; SALT_LEN];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut salt);
    let mut key = [0u8; KEY_LEN];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, ITERATIONS, &mut key);
    format!("pbkdf2$sha256${ITERATIONS}${}${}", b64(&salt), b64(&key))
}

/// Verify a password against a stored hash. Any malformed hash is a rejection,
/// never a panic: the Python version wrapped this in a bare `except` that
/// turned a corrupted hash into "wrong password" silently, which is the same
/// outcome but without hiding why.
pub fn verify_password(password: &str, stored: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some("pbkdf2"), Some("sha256")) = (parts.next(), parts.next()) else {
        return false;
    };
    let Some(Ok(iterations)) = parts.next().map(str::parse::<u32>) else {
        return false;
    };
    let (Some(salt), Some(expected)) = (parts.next().and_then(unb64), parts.next().and_then(unb64))
    else {
        return false;
    };
    if expected.is_empty() {
        return false;
    }

    let mut key = vec![0u8; expected.len()];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut key);
    key.ct_eq(&expected).into()
}

/// 32 bytes of OS randomness, hex encoded.
pub fn random_secret() -> String {
    let mut buf = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn sign(payload: &str, secret: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    b64(&mac.finalize().into_bytes())
}

/// `base64(issued_at).base64url(hmac_sha256(payload, secret))`
pub fn issue_session(secret: &[u8]) -> String {
    let issued = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let payload = b64(issued.to_string().as_bytes());
    let signature = sign(&payload, secret);
    format!("{payload}.{signature}")
}

/// Verify signature then expiry. Signature first, so a tampered timestamp is
/// rejected before it can influence anything.
pub fn verify_session(token: &str, secret: &[u8]) -> bool {
    let Some((payload, signature)) = token.split_once('.') else {
        return false;
    };
    let Some(sig_bytes) = unb64(signature) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    if mac.verify_slice(&sig_bytes).is_err() {
        return false;
    }

    let Some(raw) = unb64(payload) else {
        return false;
    };
    let Ok(text) = String::from_utf8(raw) else {
        return false;
    };
    let Ok(issued) = text.parse::<u64>() else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Reject the future as well as the past: a clock jump should not mint an
    // immortal session.
    issued <= now && now.saturating_sub(issued) < SESSION_TTL_SECS
}

pub fn session_from_cookie(header: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        (k.trim() == COOKIE_NAME).then(|| v.trim().to_string())
    })
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    password: String,
}

pub async fn login(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    axum::Json(body): axum::Json<LoginRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // One response for "wrong password" and "no password set", so the endpoint
    // cannot be used to probe which.
    if !state.verify_password(&body.password).await {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({ "success": false, "error": "Invalid password" })),
        )
            .into_response();
    }

    let token = state.issue_session().await;
    let cookie = format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={SESSION_TTL_SECS}"
    );
    (
        axum::http::StatusCode::OK,
        [(axum::http::header::SET_COOKIE, cookie)],
        axum::Json(serde_json::json!({ "success": true })),
    )
        .into_response()
}

pub async fn logout() -> axum::response::Response {
    use axum::response::IntoResponse;
    let cookie = format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    (
        axum::http::StatusCode::OK,
        [(axum::http::header::SET_COOKIE, cookie)],
        axum::Json(serde_json::json!({ "success": true })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip() {
        let h = hash_password("correct horse battery staple");
        assert!(verify_password("correct horse battery staple", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn same_password_yields_different_hashes() {
        let a = hash_password("same");
        let b = hash_password("same");
        assert_ne!(a, b, "salt must be random per hash");
        assert!(verify_password("same", &a) && verify_password("same", &b));
    }

    #[test]
    fn hash_format_is_self_describing() {
        let h = hash_password("x");
        let parts: Vec<_> = h.split('$').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], "pbkdf2");
        assert_eq!(parts[1], "sha256");
        assert_eq!(parts[2], ITERATIONS.to_string());
    }

    #[test]
    fn malformed_hashes_are_rejected_not_panicking() {
        for bad in [
            "",
            ":",
            "garbage",
            "pbkdf2$sha256$notanumber$aa$bb",
            "pbkdf2$sha256$1000$!!!$!!!",
            "pbkdf2$md5$1000$aa$bb",
            "pbkdf2$sha256$1000$aa",
            "$$$$",
        ] {
            assert!(!verify_password("x", bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn session_round_trip() {
        let secret = random_secret();
        let token = issue_session(secret.as_bytes());
        assert!(verify_session(&token, secret.as_bytes()));
    }

    #[test]
    fn session_rejects_a_different_secret() {
        let token = issue_session(b"secret-one");
        assert!(!verify_session(&token, b"secret-two"));
    }

    #[test]
    fn session_rejects_tampering() {
        let secret = random_secret();
        let token = issue_session(secret.as_bytes());
        let (payload, sig) = token.split_once('.').unwrap();

        // Forged timestamp with the original signature.
        let forged = format!("{}.{}", b64(b"9999999999"), sig);
        assert!(!verify_session(&forged, secret.as_bytes()));

        // Corrupted signature.
        let mut broken = sig.to_string();
        broken.push('x');
        assert!(!verify_session(
            &format!("{payload}.{broken}"),
            secret.as_bytes()
        ));
    }

    #[test]
    fn session_rejects_expired_and_future() {
        let secret = b"k";
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let old = b64((now - SESSION_TTL_SECS - 10).to_string().as_bytes());
        let future = b64((now + 10_000).to_string().as_bytes());
        for payload in [old, future] {
            let token = format!("{payload}.{}", sign(&payload, secret));
            assert!(
                !verify_session(&token, secret),
                "{payload} should be rejected"
            );
        }
    }

    #[test]
    fn session_rejects_junk_shapes() {
        let secret = b"k";
        for bad in ["", ".", "no-dot", "a.b.c", "!!!.???"] {
            assert!(!verify_session(bad, secret), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn cookie_parsing_picks_the_right_one() {
        assert_eq!(
            session_from_cookie("other=1; mindash_session=abc.def; x=2").as_deref(),
            Some("abc.def")
        );
        assert_eq!(session_from_cookie("nothing=here"), None);
    }
}
