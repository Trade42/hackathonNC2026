//! The bytes of a Web Push: the message encryption of RFC 8291 (the `aes128gcm` content coding of
//! RFC 8188, one record) and the VAPID signature of RFC 8292. No I/O; `sender` does the HTTP.

use std::fmt;

use aws_lc_rs::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::agreement::{self, ECDH_P256, ParsedPublicKey, PrivateKey, UnparsedPublicKey};
use aws_lc_rs::error::Unspecified;
use aws_lc_rs::hkdf::{HKDF_SHA256, KeyType, Salt};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;

use crate::config::Config;

/// An uncompressed P-256 point: 0x04, then x and y. A subscription's `p256dh`, the VAPID public key
/// and the sender's one-time key all have this form.
pub const PUBLIC_KEY_LEN: usize = 65;
/// A P-256 private key: the 32-byte big-endian scalar, as `npx web-push generate-vapid-keys` prints it.
pub const PRIVATE_KEY_LEN: usize = 32;
/// A subscription's `auth` secret.
pub const AUTH_SECRET_LEN: usize = 16;
const SALT_LEN: usize = 16;
/// The record size in the header. The whole message is one record, so it only has to be larger
/// than the message; 4096 is what RFC 8291 section 4 says push services must accept.
const RECORD_SIZE: u32 = 4096;
/// The header before the ciphertext: salt, record size, key id length and key id (the sender's
/// one-time public key). 86 bytes.
pub const HEADER_LEN: usize = SALT_LEN + 4 + 1 + PUBLIC_KEY_LEN;
const TAG_LEN: usize = 16;
/// The padding delimiter that ends the last (here the only) record.
const LAST_RECORD: u8 = 0x02;
/// A VAPID token's lifetime. RFC 8292 allows at most 24 hours.
pub const VAPID_TOKEN_SECS: u64 = 12 * 60 * 60;

/// Base64url without padding, the form browsers and VAPID use.
pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes base64url, with or without `=` padding. Standard base64 (`+` and `/`) is accepted too,
/// since key generators differ.
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let normalized: String = text
        .trim()
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    URL_SAFE_NO_PAD.decode(normalized).ok()
}

/// A subscription's `p256dh`: 65 bytes, uncompressed, and a point on P-256.
pub fn parse_ua_public(bytes: &[u8]) -> Result<[u8; PUBLIC_KEY_LEN], &'static str> {
    let key: [u8; PUBLIC_KEY_LEN] = bytes.try_into().map_err(|_| "must be 65 bytes")?;
    if key[0] != 0x04 {
        return Err("must be an uncompressed P-256 public key");
    }
    ParsedPublicKey::try_from(UnparsedPublicKey::new(&ECDH_P256, &key))
        .map_err(|_| "is not a P-256 public key")?;
    Ok(key)
}

/// A subscription's `auth`: 16 bytes.
pub fn parse_auth_secret(bytes: &[u8]) -> Result<[u8; AUTH_SECRET_LEN], &'static str> {
    bytes.try_into().map_err(|_| "must be 16 bytes")
}

/// Encrypts `plaintext` for one subscription (its `p256dh` and `auth`), with a new one-time key
/// and salt: the request body, header then ciphertext.
pub fn encrypt(
    ua_public: &[u8; PUBLIC_KEY_LEN],
    auth_secret: &[u8; AUTH_SECRET_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, Unspecified> {
    let mut salt = [0u8; SALT_LEN];
    SystemRandom::new().fill(&mut salt)?;
    let as_private = PrivateKey::generate(&ECDH_P256)?;
    encrypt_with(&as_private, &salt, ua_public, auth_secret, plaintext)
}

/// [`encrypt`] with the sender's key and the salt given, so the RFC's example can be checked.
fn encrypt_with(
    as_private: &PrivateKey,
    salt: &[u8; SALT_LEN],
    ua_public: &[u8; PUBLIC_KEY_LEN],
    auth_secret: &[u8; AUTH_SECRET_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, Unspecified> {
    let as_public = as_private.compute_public_key()?;
    let as_public = as_public.as_ref();
    let (cek, nonce) = agreement::agree(
        as_private,
        UnparsedPublicKey::new(&ECDH_P256, ua_public),
        Unspecified,
        |ecdh_secret| content_keys(ecdh_secret, auth_secret, salt, ua_public, as_public),
    )?;

    let mut body = Vec::with_capacity(HEADER_LEN + plaintext.len() + 1 + TAG_LEN);
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(PUBLIC_KEY_LEN as u8);
    body.extend_from_slice(as_public);

    let mut record = Vec::with_capacity(plaintext.len() + 1 + TAG_LEN);
    record.extend_from_slice(plaintext);
    record.push(LAST_RECORD);
    LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek)?).seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::empty(),
        &mut record,
    )?;
    body.extend_from_slice(&record);
    Ok(body)
}

/// The content encryption key and nonce (RFC 8291 section 3.4, RFC 8188 section 2.2).
fn content_keys(
    ecdh_secret: &[u8],
    auth_secret: &[u8],
    salt: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
) -> Result<([u8; 16], [u8; 12]), Unspecified> {
    // IKM = HKDF(salt = auth_secret, ikm = ecdh_secret, info = "WebPush: info" 0x00 ua as, 32).
    let mut ikm = [0u8; 32];
    Salt::new(HKDF_SHA256, auth_secret)
        .extract(ecdh_secret)
        .expand(&[b"WebPush: info\0", ua_public, as_public], Len(32))?
        .fill(&mut ikm)?;
    let prk = Salt::new(HKDF_SHA256, salt).extract(&ikm);
    let mut cek = [0u8; 16];
    prk.expand(&[b"Content-Encoding: aes128gcm\0"], Len(16))?
        .fill(&mut cek)?;
    let mut nonce = [0u8; 12];
    prk.expand(&[b"Content-Encoding: nonce\0"], Len(12))?
        .fill(&mut nonce)?;
    Ok((cek, nonce))
}

/// An HKDF output length.
struct Len(usize);

impl KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

/// Why there is no VAPID key pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VapidError {
    /// Neither VAPID_PUBLIC_KEY nor VAPID_PRIVATE_KEY is set.
    Unset,
    /// Set but unusable; the reason names the variable, never its value.
    Invalid(&'static str),
}

/// The server's VAPID key pair (RFC 8292), from VAPID_PUBLIC_KEY and VAPID_PRIVATE_KEY.
pub struct Vapid {
    key_pair: EcdsaKeyPair,
    /// The public key, base64url without padding: `PushKey.vapid_public_key` and each request's `k=`.
    public_key: String,
}

impl fmt::Debug for Vapid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vapid")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

impl Vapid {
    pub fn from_config(config: &Config) -> Result<Self, VapidError> {
        match (&config.vapid_public_key, &config.vapid_private_key) {
            (None, None) => Err(VapidError::Unset),
            (Some(public), Some(private)) => Self::from_base64url(public, private.expose()),
            _ => Err(VapidError::Invalid(
                "only one of VAPID_PUBLIC_KEY and VAPID_PRIVATE_KEY is set",
            )),
        }
    }

    /// The pair from its base64url text: the 65-byte uncompressed public key and the 32-byte
    /// private scalar. They must belong together.
    pub fn from_base64url(public: &str, private: &str) -> Result<Self, VapidError> {
        let public = b64url_decode(public)
            .filter(|k| k.len() == PUBLIC_KEY_LEN && k[0] == 0x04)
            .ok_or(VapidError::Invalid(
                "VAPID_PUBLIC_KEY is not a base64url uncompressed P-256 public key (65 bytes)",
            ))?;
        let private = b64url_decode(private)
            .filter(|k| k.len() == PRIVATE_KEY_LEN)
            .ok_or(VapidError::Invalid(
                "VAPID_PRIVATE_KEY is not a base64url P-256 private key (32 bytes)",
            ))?;
        let key_pair = EcdsaKeyPair::from_private_key_and_public_key(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            &private,
            &public,
        )
        .map_err(|_| {
            VapidError::Invalid("VAPID_PUBLIC_KEY and VAPID_PRIVATE_KEY are not one P-256 key pair")
        })?;
        Ok(Self {
            key_pair,
            public_key: b64url(&public),
        })
    }

    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// The `Authorization` header for a push to a service at `audience` (the endpoint's origin):
    /// `vapid t=<JWT>, k=<public key>`. The JWT is ES256 with `aud`, `exp`
    /// ([`VAPID_TOKEN_SECS`] after `now_unix`) and `sub` (`subject`, an https URL or `mailto:`).
    pub fn authorization(
        &self,
        audience: &str,
        subject: &str,
        now_unix: u64,
    ) -> Result<String, Unspecified> {
        let header = b64url(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = json!({"aud": audience, "exp": now_unix + VAPID_TOKEN_SECS, "sub": subject});
        let signing_input = format!("{header}.{}", b64url(claims.to_string().as_bytes()));
        let signature = self
            .key_pair
            .sign(&SystemRandom::new(), signing_input.as_bytes())?;
        Ok(format!(
            "vapid t={signing_input}.{}, k={}",
            b64url(signature.as_ref()),
            self.public_key
        ))
    }
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey as SigningPublicKey};
    use serde_json::Value;

    use super::*;

    fn b64(text: &str) -> Vec<u8> {
        b64url_decode(&text.replace([' ', '\n'], "")).unwrap()
    }

    /// The receiving browser's side, written from RFC 8291 independently of [`encrypt_with`].
    fn decrypt(ua_private: &PrivateKey, auth_secret: &[u8], body: &[u8]) -> Vec<u8> {
        let (salt, rest) = body.split_at(SALT_LEN);
        let (record_size, rest) = rest.split_at(4);
        assert_eq!(record_size, RECORD_SIZE.to_be_bytes());
        assert_eq!(rest[0] as usize, PUBLIC_KEY_LEN);
        let (as_public, ciphertext) = rest[1..].split_at(PUBLIC_KEY_LEN);
        let ua_public = ua_private.compute_public_key().unwrap();
        let (cek, nonce) = agreement::agree(
            ua_private,
            UnparsedPublicKey::new(&ECDH_P256, as_public),
            Unspecified,
            |secret| content_keys(secret, auth_secret, salt, ua_public.as_ref(), as_public),
        )
        .unwrap();
        let mut record = ciphertext.to_vec();
        let plain = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).unwrap())
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut record,
            )
            .unwrap();
        assert_eq!(plain.last(), Some(&LAST_RECORD));
        plain[..plain.len() - 1].to_vec()
    }

    /// RFC 8291 section 5 and appendix A, byte for byte.
    #[test]
    fn the_rfc_8291_example() {
        let as_private = PrivateKey::from_private_key(
            &ECDH_P256,
            &b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw"),
        )
        .unwrap();
        let ua_public: [u8; 65] = b64(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcx aOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        )
        .try_into()
        .unwrap();
        let auth: [u8; 16] = b64("BTBZMqHH6r4Tts7J_aSIgg").try_into().unwrap();
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let plaintext = b"When I grow up, I want to be a watermelon";
        assert_eq!(
            b64url(plaintext),
            "V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24"
        );

        let body = encrypt_with(&as_private, &salt, &ua_public, &auth, plaintext).unwrap();
        assert_eq!(
            body,
            b64(
                "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
                 mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT
                 pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
            )
        );
        // Header, plaintext, delimiter and tag. (The example's `Content-Length: 145` is one too
        // many: its own body decodes to 144 bytes.)
        assert_eq!(body.len(), HEADER_LEN + plaintext.len() + 1 + TAG_LEN);
        assert_eq!(body.len(), 144);
        assert_eq!(
            body[..HEADER_LEN],
            b64(
                "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z 9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
                 mlMoZIIgDll6e3vCYLocInmYWAmS6Tlz AC8wEqKK6PBru3jl7A8"
            )
        );

        // The intermediate values.
        let as_public = as_private.compute_public_key().unwrap();
        assert_eq!(
            as_public.as_ref(),
            b64(
                "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIg Dll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"
            )
        );
        let (cek, nonce) = agreement::agree(
            &as_private,
            UnparsedPublicKey::new(&ECDH_P256, &ua_public),
            Unspecified,
            |secret| {
                assert_eq!(secret, b64("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"));
                content_keys(secret, &auth, &salt, &ua_public, as_public.as_ref())
            },
        )
        .unwrap();
        assert_eq!(cek.as_slice(), b64("oIhVW04MRdy2XN9CiKLxTg"));
        assert_eq!(nonce.as_slice(), b64("4h_95klXJ5E_qnoN"));

        // And the receiver's private key opens it.
        let ua_private = PrivateKey::from_private_key(
            &ECDH_P256,
            &b64("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94"),
        )
        .unwrap();
        assert_eq!(decrypt(&ua_private, &auth, &body), plaintext);
    }

    #[test]
    fn each_message_has_its_own_key_and_salt_and_opens() {
        let ua_private = PrivateKey::generate(&ECDH_P256).unwrap();
        let ua_public: [u8; 65] = ua_private
            .compute_public_key()
            .unwrap()
            .as_ref()
            .try_into()
            .unwrap();
        let auth = [7u8; 16];
        let message = br#"{"notification":{"id":"ntf_1"}}"#;
        let one = encrypt(&ua_public, &auth, message).unwrap();
        let two = encrypt(&ua_public, &auth, message).unwrap();
        assert_ne!(one[..HEADER_LEN], two[..HEADER_LEN]);
        assert_eq!(one.len(), HEADER_LEN + message.len() + 1 + TAG_LEN);
        assert_eq!(decrypt(&ua_private, &auth, &one), message);
        assert_eq!(decrypt(&ua_private, &auth, &two), message);
    }

    #[test]
    fn subscription_keys_are_checked() {
        let good = PrivateKey::generate(&ECDH_P256)
            .unwrap()
            .compute_public_key()
            .unwrap();
        assert!(parse_ua_public(good.as_ref()).is_ok());
        assert!(parse_ua_public(&good.as_ref()[..64]).is_err());
        let mut compressed_tag = good.as_ref().to_vec();
        compressed_tag[0] = 0x02;
        assert!(parse_ua_public(&compressed_tag).is_err());
        let mut off_curve = good.as_ref().to_vec();
        off_curve[64] ^= 1;
        assert!(parse_ua_public(&off_curve).is_err());
        assert!(parse_auth_secret(&[0; 16]).is_ok());
        assert!(parse_auth_secret(&[0; 15]).is_err());

        assert_eq!(b64url_decode("BTBZMqHH6r4Tts7J_aSIgg==").unwrap().len(), 16);
        assert_eq!(b64url_decode("BTBZMqHH6r4Tts7J/aSIgg").unwrap().len(), 16);
        assert!(b64url_decode("not base64!").is_none());
    }

    /// A pair as `npx web-push generate-vapid-keys` prints it: base64url public point and scalar.
    fn generated_pair() -> (String, String) {
        loop {
            let mut scalar = [0u8; 32];
            SystemRandom::new().fill(&mut scalar).unwrap();
            if let Ok(private) = PrivateKey::from_private_key(&ECDH_P256, &scalar) {
                let public = private.compute_public_key().unwrap();
                return (b64url(public.as_ref()), b64url(&scalar));
            }
        }
    }

    #[test]
    fn vapid_signs_an_es256_token_for_the_audience() {
        let (public, private) = generated_pair();
        let vapid = Vapid::from_base64url(&public, &private).unwrap();
        assert_eq!(vapid.public_key(), public);

        let header = vapid
            .authorization("https://fcm.googleapis.com", "https://ferry.example", 1_000)
            .unwrap();
        let (token, key) = header
            .strip_prefix("vapid t=")
            .unwrap()
            .split_once(", k=")
            .unwrap();
        assert_eq!(key, public);
        let (signing_input, signature) = token.rsplit_once('.').unwrap();
        let (head, claims) = signing_input.split_once('.').unwrap();
        let head: Value = serde_json::from_slice(&b64(head)).unwrap();
        assert_eq!(head, json!({"typ": "JWT", "alg": "ES256"}));
        let claims: Value = serde_json::from_slice(&b64(claims)).unwrap();
        assert_eq!(
            claims,
            json!({"aud": "https://fcm.googleapis.com", "exp": 1_000 + VAPID_TOKEN_SECS, "sub": "https://ferry.example"})
        );
        let signature = b64(signature);
        assert_eq!(signature.len(), 64, "ES256 is r || s");
        SigningPublicKey::new(&ECDSA_P256_SHA256_FIXED, b64(&public))
            .verify(signing_input.as_bytes(), &signature)
            .unwrap();
    }

    #[test]
    fn vapid_keys_must_be_one_pair() {
        let (public, private) = generated_pair();
        let (other_public, _) = generated_pair();
        assert!(matches!(
            Vapid::from_base64url(&other_public, &private),
            Err(VapidError::Invalid(_))
        ));
        assert!(matches!(
            Vapid::from_base64url(&public, "short"),
            Err(VapidError::Invalid(_))
        ));
        assert!(matches!(
            Vapid::from_base64url(&private, &private),
            Err(VapidError::Invalid(_))
        ));
        // Padded or standard base64 is accepted, and the key comes back as base64url.
        let padded = format!("{}=", public.replace('-', "+").replace('_', "/"));
        assert_eq!(
            Vapid::from_base64url(&padded, &private)
                .unwrap()
                .public_key(),
            public
        );
        // Debug never shows the private key.
        let debug = format!("{:?}", Vapid::from_base64url(&public, &private).unwrap());
        assert!(!debug.contains(&private));
    }

    #[test]
    fn vapid_from_config() {
        let (public, private) = generated_pair();
        let config = |pairs: &[(&str, &str)]| {
            let mut vars: std::collections::HashMap<String, String> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            vars.insert("DATABASE_URL".into(), "postgres://u:p@127.0.0.1/db".into());
            Config::from_vars(&vars).unwrap()
        };
        assert_eq!(
            Vapid::from_config(&config(&[])).unwrap_err(),
            VapidError::Unset
        );
        assert!(matches!(
            Vapid::from_config(&config(&[("VAPID_PUBLIC_KEY", &public)])),
            Err(VapidError::Invalid(_))
        ));
        assert_eq!(
            Vapid::from_config(&config(&[
                ("VAPID_PUBLIC_KEY", &public),
                ("VAPID_PRIVATE_KEY", &private)
            ]))
            .unwrap()
            .public_key(),
            public
        );
    }
}
