use age::x25519::{Identity, Recipient};
use argon2::{Algorithm, Argon2, Params, Version};
use bech32::{ToBase32, Variant};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::{Read, Write};
use std::str::FromStr;

pub struct Keys {
    pub identity: Identity,
    pub recipient: Recipient,
    pub hmac_key: [u8; 32],
}

/// Argon2id(m=65536 KiB, t=3, p=1) → 64 bytes: first 32 (RFC 7748-clamped)
/// become the x25519 identity scalar, last 32 the HMAC object-naming key.
pub fn derive(passphrase: &str, salt: &[u8; 32]) -> Keys {
    let params = Params::new(65536, 3, 1, Some(64)).expect("fixed argon2 params are valid");
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 64];
    a2.hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .expect("argon2 with fixed params cannot fail");
    let mut sk = [0u8; 32];
    sk.copy_from_slice(&out[..32]);
    sk[0] &= 248;
    sk[31] &= 127;
    sk[31] |= 64;
    // age 0.10 exposes no raw-scalar constructor; go through its bech32 format.
    let encoded = bech32::encode("age-secret-key-", sk.to_base32(), Variant::Bech32)
        .expect("fixed HRP is valid");
    let identity = Identity::from_str(&encoded).expect("32-byte bech32 key is valid");
    let recipient = identity.to_public();
    let mut hmac_key = [0u8; 32];
    hmac_key.copy_from_slice(&out[32..]);
    Keys {
        identity,
        recipient,
        hmac_key,
    }
}

/// gzip(level 6) then age-encrypt to the recipient.
pub fn seal(plain: &[u8], r: &Recipient) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    gz.write_all(plain).expect("in-memory gzip cannot fail");
    let compressed = gz.finish().expect("in-memory gzip cannot fail");
    let encryptor = age::Encryptor::with_recipients(vec![Box::new(r.clone())])
        .expect("one recipient is always provided");
    let mut sealed = Vec::new();
    let mut w = encryptor
        .wrap_output(&mut sealed)
        .expect("in-memory write cannot fail");
    w.write_all(&compressed)
        .expect("in-memory write cannot fail");
    w.finish().expect("in-memory write cannot fail");
    sealed
}

pub fn open(sealed: &[u8], id: &Identity) -> anyhow::Result<Vec<u8>> {
    let decryptor = match age::Decryptor::new(sealed)? {
        age::Decryptor::Recipients(d) => d,
        age::Decryptor::Passphrase(_) => anyhow::bail!("unexpected passphrase-sealed payload"),
    };
    let mut reader = decryptor.decrypt(std::iter::once(id as &dyn age::Identity))?;
    let mut compressed = Vec::new();
    reader.read_to_end(&mut compressed)?;
    let mut gz = flate2::read::GzDecoder::new(&compressed[..]);
    let mut plain = Vec::new();
    gz.read_to_end(&mut plain)?;
    Ok(plain)
}

/// Lowercase-hex HMAC-SHA256 of the portable path, keyed by the derived key —
/// object names reveal nothing to a repo observer and dodge Windows reserved
/// names / case-fold collisions.
pub fn object_name(hmac_key: &[u8; 32], portable_path: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(hmac_key).expect("HMAC accepts any key length");
    mac.update(portable_path.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_passphrase_same_keys_across_devices() {
        let salt = [7u8; 32];
        let a = derive("hunter2", &salt);
        let b = derive("hunter2", &salt);
        assert_eq!(a.recipient.to_string(), b.recipient.to_string());
        assert_eq!(a.hmac_key, b.hmac_key);
        assert_ne!(
            derive("hunter2", &[8u8; 32]).recipient.to_string(),
            a.recipient.to_string()
        );
    }

    #[test]
    fn seal_open_roundtrip_and_nondeterminism() {
        let k = derive("pw", &[1u8; 32]);
        let msg = b"hello \xEA\xB0\x80"; // includes UTF-8 Korean bytes
        let s1 = seal(msg, &k.recipient);
        let s2 = seal(msg, &k.recipient);
        assert_ne!(s1, s2); // age is non-deterministic — this is WHY state.json tracks plaintext hashes
        assert_eq!(open(&s1, &k.identity).unwrap(), msg.to_vec());
    }

    #[test]
    fn wrong_passphrase_fails_closed() {
        let k1 = derive("pw", &[1u8; 32]);
        let k2 = derive("pw2", &[1u8; 32]);
        assert!(open(&seal(b"x", &k1.recipient), &k2.identity).is_err());
    }

    #[test]
    fn object_names_stable_and_keyed() {
        let k = derive("pw", &[1u8; 32]);
        let n1 = object_name(&k.hmac_key, "settings.json");
        assert_eq!(n1, object_name(&k.hmac_key, "settings.json"));
        assert_ne!(
            n1,
            object_name(&derive("other", &[1u8; 32]).hmac_key, "settings.json")
        );
        assert!(n1.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
