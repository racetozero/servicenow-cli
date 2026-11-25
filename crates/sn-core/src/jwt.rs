//! The signed assertion of the OAuth JWT bearer grant (RFC 7523).

use crate::config::OAuthConfig;
use crate::error::{Error, Result};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, RsaKeyPair};
use serde_json::json;
use std::path::Path;

const LIFETIME_SECS: u64 = 300;

enum Key {
    Rsa(RsaKeyPair),
    Ec(EcdsaKeyPair),
}

struct Signer {
    key: Key,
    alg: &'static str,
}

/// Loads the key now, so that a bad file fails at sign-in time.
pub fn check_key(config: &OAuthConfig) -> Result<()> {
    load(config).map(|_| ())
}

fn load(config: &OAuthConfig) -> Result<Signer> {
    let path = config.jwt_key_file.as_deref().ok_or_else(|| {
        Error::invalid("the JWT grant needs a private key; pass --jwt-key-file FILE (PEM)")
    })?;
    let pem = std::fs::read_to_string(path).map_err(|err| {
        Error::invalid(format!(
            "cannot read the JWT key {} ({err}); pass a PEM file you can read",
            path.display()
        ))
    })?;
    parse(path, &pem, config.jwt_alg.as_deref())
}

fn parse(path: &Path, pem: &str, alg: Option<&str>) -> Result<Signer> {
    let bad = |why: &str| {
        Error::invalid(format!(
            "the JWT key {} {why}; pass an unencrypted RSA (2048 bits or more) or EC P-256/P-384 key, converted with `openssl pkcs8 -topk8 -nocrypt -in KEY -out NEW`",
            path.display()
        ))
    };
    if pem.contains("ENCRYPTED") {
        return Err(bad("is encrypted"));
    }
    let label = pem
        .lines()
        .find_map(|l| l.trim().strip_prefix("-----BEGIN ")?.strip_suffix("-----"))
        .filter(|l| l.ends_with("PRIVATE KEY"))
        .ok_or_else(|| bad("is not a PEM private key"))?;
    let body: String = pem
        .lines()
        .skip_while(|l| !l.contains("-----BEGIN"))
        .skip(1)
        .take_while(|l| !l.contains("-----END"))
        .map(str::trim)
        .collect();
    let der = STANDARD
        .decode(body)
        .map_err(|_| bad("is not valid base64"))?;
    let rsa_alg = || match alg {
        None | Some("RS256") => Ok("RS256"),
        Some("RS384") => Ok("RS384"),
        Some("RS512") => Ok("RS512"),
        Some(_) => Err(bad("is an RSA key, which signs RS256, RS384 or RS512")),
    };
    if label == "RSA PRIVATE KEY" {
        let pair = RsaKeyPair::from_der(&der).map_err(|_| bad("is not a usable RSA key"))?;
        return Ok(Signer {
            key: Key::Rsa(pair),
            alg: rsa_alg()?,
        });
    }
    if label != "PRIVATE KEY" {
        return Err(bad(&format!("has the format `{label}`")));
    }
    if let Ok(pair) = RsaKeyPair::from_pkcs8(&der) {
        return Ok(Signer {
            key: Key::Rsa(pair),
            alg: rsa_alg()?,
        });
    }
    let rng = SystemRandom::new();
    for (name, spec) in [
        ("ES256", &signature::ECDSA_P256_SHA256_FIXED_SIGNING),
        ("ES384", &signature::ECDSA_P384_SHA384_FIXED_SIGNING),
    ] {
        if alg.is_none_or(|wanted| wanted == name)
            && let Ok(pair) = EcdsaKeyPair::from_pkcs8(spec, &der, &rng)
        {
            return Ok(Signer {
                key: Key::Ec(pair),
                alg: name,
            });
        }
    }
    Err(bad(
        "holds a key type that cannot sign the requested algorithm",
    ))
}

impl Signer {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>> {
        let rng = SystemRandom::new();
        let failed = |_| Error::Internal("signing the JWT failed".into());
        match &self.key {
            Key::Rsa(pair) => {
                let padding: &dyn signature::RsaEncoding = match self.alg {
                    "RS384" => &signature::RSA_PKCS1_SHA384,
                    "RS512" => &signature::RSA_PKCS1_SHA512,
                    _ => &signature::RSA_PKCS1_SHA256,
                };
                let mut out = vec![0; pair.public().modulus_len()];
                pair.sign(padding, &rng, message, &mut out)
                    .map_err(failed)?;
                Ok(out)
            }
            Key::Ec(pair) => Ok(pair.sign(&rng, message).map_err(failed)?.as_ref().to_vec()),
        }
    }
}

/// A fresh assertion: the instance checks `iss`, `aud`, `sub`, `exp`, `jti`.
pub fn assertion(config: &OAuthConfig) -> Result<String> {
    let signer = load(config)?;
    let subject = config.jwt_subject.as_deref().ok_or_else(|| {
        Error::invalid("the JWT grant needs the user to act as; pass --jwt-subject USER")
    })?;
    let now = crate::now_unix();
    let mut header = json!({ "alg": signer.alg, "typ": "JWT" });
    if let Some(kid) = &config.jwt_kid {
        header["kid"] = kid.clone().into();
    }
    let claims = json!({
        "iss": config.client_id, "aud": config.client_id, "sub": subject,
        "iat": now, "exp": now + LIFETIME_SECS, "jti": crate::oauth::random_urlsafe(16)?,
    });
    let encode = |v: &serde_json::Value| URL_SAFE_NO_PAD.encode(v.to_string());
    let message = format!("{}.{}", encode(&header), encode(&claims));
    let signature = URL_SAFE_NO_PAD.encode(signer.sign(message.as_bytes())?);
    Ok(format!("{message}.{signature}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    #[test]
    fn ec_assertion_verifies_and_rejects_bad_keys() {
        let rng = SystemRandom::new();
        let spec = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let der = EcdsaKeyPair::generate_pkcs8(spec, &rng).expect("key");
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("k.pem");
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(der.as_ref())
        );
        std::fs::write(&path, &pem).expect("write");
        let config = OAuthConfig {
            client_id: "cid".into(),
            jwt_key_file: Some(path.clone()),
            jwt_subject: Some("svc".into()),
            jwt_kid: Some("k1".into()),
            ..OAuthConfig::default()
        };
        let token = assertion(&config).expect("assertion");
        let (message, sig) = token.rsplit_once('.').expect("parts");
        let pair = EcdsaKeyPair::from_pkcs8(spec, der.as_ref(), &rng).expect("pair");
        let public = signature::UnparsedPublicKey::new(
            &signature::ECDSA_P256_SHA256_FIXED,
            pair.public_key().as_ref().to_vec(),
        );
        public
            .verify(
                message.as_bytes(),
                &URL_SAFE_NO_PAD.decode(sig).expect("sig"),
            )
            .expect("valid");
        assert!(parse(&path, &pem, Some("RS256")).is_err());
        assert!(parse(&path, "-----BEGIN ENCRYPTED PRIVATE KEY-----", None).is_err());
        assert!(parse(&path, "nothing", None).is_err());
    }
}
