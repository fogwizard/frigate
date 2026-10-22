//! HMAC-SHA256 signed FLV playback URLs for WeChat mini-program / Nginx auth.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use vigil_common::StreamConfig;

type HmacSha256 = Hmac<Sha256>;

fn message(camera: &str, exp_unix: i64) -> String {
    format!("{camera}\n{exp_unix}")
}

pub fn sign(secret: &str, camera: &str, exp_unix: i64) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(message(camera, exp_unix).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub fn verify(secret: &str, camera: &str, exp_unix: i64, sig: &str) -> bool {
    let expected = sign(secret, camera, exp_unix);
    // Constant-time-ish compare via hmac verify on re-computed bytes.
    if expected.len() != sig.len() {
        return false;
    }
    expected
        .bytes()
        .zip(sig.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

pub struct SignedPlay {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

pub fn issue_play_url(cfg: &StreamConfig, camera: &str, now: DateTime<Utc>) -> SignedPlay {
    let exp_unix = now.timestamp() + cfg.sign_ttl_secs as i64;
    let url = cfg.play_url(camera);
    let expires_at = DateTime::from_timestamp(exp_unix, 0).unwrap_or(now);
    SignedPlay { url, expires_at }
}

pub fn check_auth(
    cfg: &StreamConfig,
    camera: &str,
    exp_unix: i64,
    sig: &str,
    now: DateTime<Utc>,
) -> Result<(), AuthFail> {
    if now.timestamp() > exp_unix {
        return Err(AuthFail::Expired);
    }
    if !verify(&cfg.sign_secret, camera, exp_unix, sig) {
        return Err(AuthFail::BadSig);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum AuthFail {
    Expired,
    BadSig,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn sign_verify_roundtrip() {
        let sig = sign("secret", "cam1", 1_700_000_000);
        assert!(verify("secret", "cam1", 1_700_000_000, &sig));
        assert!(!verify("secret", "cam2", 1_700_000_000, &sig));
        assert!(!verify("other", "cam1", 1_700_000_000, &sig));
    }

    #[test]
    fn play_url_is_go2rtc_flv() {
        let cfg = StreamConfig::default();
        let now = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let play = issue_play_url(&cfg, "cam1", now);
        assert_eq!(play.url, "http://127.0.0.1:1984/api/stream.flv?src=cam1");
    }

    #[test]
    fn auth_rejects_expired() {
        let cfg = StreamConfig {
            sign_secret: "secret".into(),
            sign_ttl_secs: 60,
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let exp = now.timestamp() - 1;
        let sig = sign("secret", "cam1", exp);
        assert!(matches!(
            check_auth(&cfg, "cam1", exp, &sig, now),
            Err(AuthFail::Expired)
        ));
    }
}
