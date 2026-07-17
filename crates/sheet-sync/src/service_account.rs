//! Non-interactive Google service-account auth: RS256-signed JWT bearer
//! grant (RFC 7523 / [Google's OAuth2 service-account flow][gsa]), no
//! browser and no loopback listener required — the counterpart to
//! [`crate::oauth`]'s interactive installed-app flow.
//!
//! [gsa]: https://developers.google.com/identity/protocols/oauth2/service-account
//!
//! As with [`crate::oauth`], all branching logic lives in *pure* helpers
//! ([`parse_service_account_json`], [`build_jwt`], `pkcs8_pem_to_der`) that
//! are unit-tested without any network access; [`access_token`] is thin glue
//! over those helpers plus `ureq`/`SystemTime`.
//!
//! ## Secrets
//!
//! The private key and the signed JWT (which is a bearer credential until
//! it expires) must never appear in an `anyhow` error message or log line —
//! see `crate::oauth::redact_token_fields` (crate-private), reused here on
//! every HTTP error body this module surfaces.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use serde::Deserialize;

use crate::oauth::AccessToken;

/// The Sheets API scope this crate requests (read + write cell values),
/// mirroring `crate::oauth::SHEETS_SCOPE` (private to that module, so
/// duplicated here rather than exposed cross-module for one constant).
const SHEETS_SCOPE: &str = "https://www.googleapis.com/auth/spreadsheets";

/// The JWT-bearer grant lifetime Google's token endpoint accepts: an hour,
/// per [RFC 7523 §3](https://www.rfc-editor.org/rfc/rfc7523#section-3) and
/// Google's own documented maximum.
const JWT_LIFETIME_SECS: u64 = 3600;

/// Per-request timeout for the token-endpoint call, matching
/// `crate::oauth::HTTP_TIMEOUT`.
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A parsed Google service-account key: the identity + endpoint fields
/// [`build_jwt`]/[`access_token`] need to mint and exchange a JWT-bearer
/// grant. Deliberately does not carry the raw JSON's other fields
/// (`project_id`, `private_key_id`, `client_id`, ...) — nothing downstream
/// of this crate needs them.
///
/// `Debug` is hand-written rather than derived — see the manual `impl`
/// below — because `private_key_pem` is a bearer secret that must never
/// land in a log line or error message via an incidental `{:?}`.
#[derive(Clone)]
pub struct ServiceAccount {
    pub client_email: String,
    pub private_key_pem: String,
    pub token_uri: String,
}

/// Redacts `private_key_pem`: only `client_email`/`token_uri` (both
/// non-secret identifiers) are printed, matching this module's doc-comment
/// guarantee that the private key never appears in a log line or error
/// message.
impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccount")
            .field("client_email", &self.client_email)
            .field("private_key_pem", &"<redacted>")
            .field("token_uri", &self.token_uri)
            .finish()
    }
}

/// The subset of a Google service-account key JSON's fields this crate
/// reads. `#[serde(deny_unknown_fields)]` is deliberately *not* set — a
/// service-account key has several other fields (`project_id`,
/// `private_key_id`, `client_id`, `auth_uri`, ...) this crate has no use
/// for, and rejecting them would make this parser needlessly brittle
/// against a file this crate didn't generate.
#[derive(Debug, Deserialize)]
struct ServiceAccountFile {
    #[serde(rename = "type")]
    type_: Option<String>,
    client_email: Option<String>,
    private_key: Option<String>,
    token_uri: Option<String>,
}

/// Parses a Google service-account key JSON body (as downloaded from
/// Cloud Console → IAM & Admin → Service Accounts → Keys). Pure — no
/// filesystem, no network — so this is unit-testable directly.
///
/// Fails loud, naming the specific problem, if: the JSON is malformed;
/// `"type"` isn't `"service_account"` (this is a different credential
/// shape, e.g. an OAuth `client_secret.json`, and signing a JWT against it
/// would silently produce a token Google will just reject); or any of
/// `client_email`/`private_key`/`token_uri` is missing.
pub fn parse_service_account_json(body: &str) -> anyhow::Result<ServiceAccount> {
    let file: ServiceAccountFile = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("service_account: invalid service-account JSON: {e}"))?;

    let type_ = file.type_.ok_or_else(|| {
        anyhow::anyhow!("service_account: service-account JSON is missing required field `type`")
    })?;
    if type_ != "service_account" {
        anyhow::bail!(
            "service_account: expected `type: \"service_account\"`, got {type_:?} — is this a Google service-account key JSON (Cloud Console -> IAM & Admin -> Service Accounts -> Keys), not an OAuth client_secret.json?"
        );
    }

    let client_email = file.client_email.ok_or_else(|| {
        anyhow::anyhow!(
            "service_account: service-account JSON is missing required field `client_email`"
        )
    })?;
    let private_key_pem = file.private_key.ok_or_else(|| {
        anyhow::anyhow!(
            "service_account: service-account JSON is missing required field `private_key`"
        )
    })?;
    let token_uri = file.token_uri.ok_or_else(|| {
        anyhow::anyhow!(
            "service_account: service-account JSON is missing required field `token_uri`"
        )
    })?;

    Ok(ServiceAccount {
        client_email,
        private_key_pem,
        token_uri,
    })
}

/// Decodes a PKCS#8 PEM (`-----BEGIN PRIVATE KEY-----` ... base64 body ...
/// `-----END PRIVATE KEY-----`) into its inner DER bytes, by collecting
/// every line that does not start with `-----` and base64-decoding the
/// concatenation. Pure; unit-tested directly (also exercised indirectly by
/// every [`build_jwt`] test, since it's the first step of signing).
fn pkcs8_pem_to_der(pem: &str) -> anyhow::Result<Vec<u8>> {
    let b64: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    STANDARD
        .decode(b64)
        .map_err(|e| anyhow::anyhow!("service_account: invalid PKCS#8 PEM private key: {e}"))
}

/// Builds and RS256-signs a Google service-account JWT-bearer assertion
/// (header `{"alg":"RS256","typ":"JWT"}`, claims
/// `{"iss","scope","aud","iat","exp"}`) for `sa`, using `iat` as the
/// issued-at time. Deterministic given `iat`, so this is unit-testable
/// offline without a real clock or network — see the `access_token` doc
/// for how the live caller supplies `iat`.
///
/// Signs with `ring::signature::RsaKeyPair` (RS256 / PKCS#1 v1.5 over
/// SHA-256) rather than the RustCrypto `rsa` crate — `rsa` fails
/// `cargo deny` (RUSTSEC-2023-0071, a Marvin-attack timing side channel);
/// `ring` is constant-time, carries no such advisory, and is already in
/// this workspace's dependency tree transitively via
/// `ureq -> rustls -> ring`.
pub fn build_jwt(sa: &ServiceAccount, iat: u64) -> anyhow::Result<String> {
    let header_json = r#"{"alg":"RS256","typ":"JWT"}"#.to_string();
    let claims_json = serde_json::json!({
        "iss": sa.client_email,
        "scope": SHEETS_SCOPE,
        "aud": sa.token_uri,
        "iat": iat,
        "exp": iat + JWT_LIFETIME_SECS,
    })
    .to_string();

    let b64u = |b: &[u8]| URL_SAFE_NO_PAD.encode(b);
    let signing_input = format!(
        "{}.{}",
        b64u(header_json.as_bytes()),
        b64u(claims_json.as_bytes())
    );

    let der = pkcs8_pem_to_der(&sa.private_key_pem)?;
    let key_pair = ring::signature::RsaKeyPair::from_pkcs8(&der).map_err(|e| {
        anyhow::anyhow!("service_account: invalid service-account private key: {e}")
    })?;
    let rng = ring::rand::SystemRandom::new();
    let mut sig = vec![0u8; key_pair.public().modulus_len()];
    key_pair
        .sign(
            &ring::signature::RSA_PKCS1_SHA256,
            &rng,
            signing_input.as_bytes(),
            &mut sig,
        )
        .map_err(|_| anyhow::anyhow!("service_account: JWT signing failed"))?;

    Ok(format!("{signing_input}.{}", b64u(&sig)))
}

/// One `ureq::Agent` config for the token-endpoint call, matching
/// `crate::oauth`/`crate::sheets::google`'s 30s-timeout,
/// `http_status_as_error(false)` pattern (see those modules' doc comments
/// for why the latter is essential rather than cosmetic here too).
fn build_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

/// Exchanges a fresh JWT-bearer assertion for `sa` at `sa.token_uri` for a
/// short-lived [`AccessToken`] — the non-interactive, headless counterpart
/// to [`crate::oauth::access_token`]. Never logs the private key or the
/// signed JWT (a bearer credential in its own right until it expires); any
/// HTTP error body is redacted via `crate::oauth::redact_token_fields`
/// (crate-private) before it's included in the returned error.
pub fn access_token(sa: &ServiceAccount) -> anyhow::Result<AccessToken> {
    let iat = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| anyhow::anyhow!("service_account: system clock before UNIX epoch: {e}"))?
        .as_secs();
    let jwt = build_jwt(sa, iat)?;

    let agent = build_agent();
    let form = [
        ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
        ("assertion", jwt.as_str()),
    ];

    let mut resp = agent.post(&sa.token_uri).send_form(form).map_err(|e| {
        anyhow::anyhow!(
            "service_account: token request to {} failed: {e}",
            sa.token_uri
        )
    })?;

    let status = resp.status();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| anyhow::anyhow!("service_account: failed to read token response body: {e}"))?;

    if !status.is_success() {
        anyhow::bail!(
            "service_account: token request to {} returned HTTP {}: {}",
            sa.token_uri,
            status.as_u16(),
            crate::oauth::redact_token_fields(&body.chars().take(500).collect::<String>())
        );
    }

    let token_response = crate::oauth::parse_token_response(&body)?;
    Ok(AccessToken {
        token: token_response.access_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_KEY_PEM: &str = include_str!("../tests/fixtures/test_only_rsa_key.pem");

    fn valid_sa_json() -> String {
        // The PEM fixture has embedded newlines; a real Google service-
        // account JSON escapes them as literal `\n` inside the JSON string
        // (Google's own downloaded key files do this), so the test fixture
        // must too rather than relying on JSON's raw-newline-in-string
        // leniency, to match real-world input.
        let escaped_pem = FIXTURE_KEY_PEM.replace('\n', "\\n");
        format!(
            r#"{{
                "type": "service_account",
                "client_email": "t@x.iam.gserviceaccount.com",
                "private_key": "{escaped_pem}",
                "token_uri": "https://oauth2.googleapis.com/token"
            }}"#
        )
    }

    #[test]
    fn parse_service_account_json_reads_valid_key() {
        let sa = parse_service_account_json(&valid_sa_json()).expect("valid SA json");
        assert_eq!(sa.client_email, "t@x.iam.gserviceaccount.com");
        assert_eq!(sa.token_uri, "https://oauth2.googleapis.com/token");
        assert!(sa.private_key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn parse_service_account_json_rejects_wrong_type() {
        let body = r#"{"type":"authorized_user","client_email":"t@x.iam","private_key":"x","token_uri":"y"}"#;
        let err = parse_service_account_json(body).expect_err("wrong type should fail");
        assert!(err.to_string().contains("service_account"));
        assert!(err.to_string().contains("authorized_user"));
    }

    #[test]
    fn parse_service_account_json_rejects_missing_type() {
        let body = r#"{"client_email":"t@x.iam","private_key":"x","token_uri":"y"}"#;
        let err = parse_service_account_json(body).expect_err("missing type should fail");
        assert!(err.to_string().contains("type"));
    }

    #[test]
    fn parse_service_account_json_rejects_missing_private_key() {
        let body = r#"{"type":"service_account","client_email":"t@x.iam","token_uri":"https://y"}"#;
        let err = parse_service_account_json(body).expect_err("missing private_key should fail");
        assert!(err.to_string().contains("private_key"));
    }

    #[test]
    fn parse_service_account_json_rejects_missing_client_email() {
        let body = r#"{"type":"service_account","private_key":"x","token_uri":"https://y"}"#;
        let err = parse_service_account_json(body).expect_err("missing client_email should fail");
        assert!(err.to_string().contains("client_email"));
    }

    #[test]
    fn parse_service_account_json_rejects_missing_token_uri() {
        let body = r#"{"type":"service_account","client_email":"t@x.iam","private_key":"x"}"#;
        let err = parse_service_account_json(body).expect_err("missing token_uri should fail");
        assert!(err.to_string().contains("token_uri"));
    }

    #[test]
    fn parse_service_account_json_rejects_garbage() {
        let err = parse_service_account_json("not json").expect_err("garbage should fail");
        assert!(err.to_string().contains("invalid service-account JSON"));
    }

    fn test_sa() -> ServiceAccount {
        ServiceAccount {
            client_email: "t@x.iam".to_string(),
            private_key_pem: FIXTURE_KEY_PEM.to_string(),
            token_uri: "https://oauth2.googleapis.com/token".to_string(),
        }
    }

    /// The key guarantee this whole module exists for: an offline,
    /// no-network round trip proving `build_jwt`'s RS256 signature is
    /// cryptographically valid — build a JWT against the throwaway fixture
    /// key, decode header/claims, and independently re-verify the
    /// signature via `ring::signature::UnparsedPublicKey` (the same
    /// verification primitive a relying party like Google's token endpoint
    /// would use).
    #[test]
    fn build_jwt_offline_rs256_round_trip_verifies() {
        let sa = test_sa();
        let iat = 1_700_000_000u64;
        let jwt = build_jwt(&sa, iat).expect("signing succeeds with a valid PKCS#8 key");

        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must be header.claims.signature");

        let header_bytes = URL_SAFE_NO_PAD
            .decode(parts[0])
            .expect("header is valid base64url");
        let header: serde_json::Value =
            serde_json::from_slice(&header_bytes).expect("header is valid JSON");
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["typ"], "JWT");

        let claims_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .expect("claims is valid base64url");
        let claims: serde_json::Value =
            serde_json::from_slice(&claims_bytes).expect("claims is valid JSON");
        assert_eq!(claims["iss"], "t@x.iam");
        assert_eq!(claims["scope"], SHEETS_SCOPE);
        assert_eq!(claims["aud"], "https://oauth2.googleapis.com/token");
        assert_eq!(claims["iat"], iat);
        assert_eq!(claims["exp"], iat + 3600);

        let sig = URL_SAFE_NO_PAD
            .decode(parts[2])
            .expect("signature is valid base64url");
        let signing_input = format!("{}.{}", parts[0], parts[1]);

        let der = pkcs8_pem_to_der(&sa.private_key_pem).expect("PEM decodes");
        let key_pair =
            ring::signature::RsaKeyPair::from_pkcs8(&der).expect("fixture key is valid PKCS#8");
        let pub_der = key_pair.public().as_ref();
        let verified = ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            pub_der,
        )
        .verify(signing_input.as_bytes(), &sig);
        assert!(
            verified.is_ok(),
            "signature must independently verify against the fixture key's public half"
        );
    }

    #[test]
    fn pkcs8_pem_to_der_decodes_fixture_key() {
        let der = pkcs8_pem_to_der(FIXTURE_KEY_PEM).expect("fixture PEM decodes");
        // A 2048-bit RSA PKCS#8 DER key is comfortably over a kilobyte;
        // this is a coarse sanity check that decoding produced real bytes,
        // not an empty/garbage buffer.
        assert!(der.len() > 512);
    }

    #[test]
    fn service_account_debug_redacts_private_key_but_keeps_client_email() {
        let sa = test_sa();
        let debug_output = format!("{sa:?}");
        assert!(
            !debug_output.contains(FIXTURE_KEY_PEM),
            "Debug output must not contain the raw private key body: {debug_output}"
        );
        assert!(
            !debug_output.contains("BEGIN PRIVATE KEY"),
            "Debug output must not contain the PEM key markers: {debug_output}"
        );
        assert!(
            debug_output.contains("t@x.iam"),
            "Debug output should still identify the service account by client_email: {debug_output}"
        );
        assert!(debug_output.contains("<redacted>"));
    }

    #[test]
    fn pkcs8_pem_to_der_rejects_non_base64() {
        let bad = "-----BEGIN PRIVATE KEY-----\nnot valid base64!!!\n-----END PRIVATE KEY-----";
        let err = pkcs8_pem_to_der(bad).expect_err("invalid base64 should fail");
        assert!(err.to_string().contains("invalid PKCS#8 PEM"));
    }
}
