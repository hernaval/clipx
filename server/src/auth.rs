use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use uuid::Uuid;

use crate::models::Claims;

pub struct AuthService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    ttl_hours: i64,
}

impl AuthService {
    pub fn new(secret: &[u8], ttl_hours: i64) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret),
            decoding_key: DecodingKey::from_secret(secret),
            ttl_hours,
        }
    }

    pub fn create_jwt(&self, user_id: Uuid, device_id: Uuid) -> anyhow::Result<String> {
        let now = Utc::now();
        let exp = now + Duration::hours(self.ttl_hours);

        let claims = Claims {
            sub: device_id.to_string(),
            user_id: user_id.to_string(),
            device_id: device_id.to_string(),
            exp: exp.timestamp() as usize,
            iat: now.timestamp() as usize,
        };

        let token = encode(&Header::default(), &claims, &self.encoding_key)?;
        Ok(token)
    }

    pub fn validate_jwt(&self, token: &str) -> anyhow::Result<Claims> {
        let data = decode::<Claims>(token, &self.decoding_key, &Validation::default())?;
        Ok(data.claims)
    }

    pub fn generate_pairing_token() -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(&bytes)
    }
}

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwt_roundtrip() {
        let auth = AuthService::new(b"test-secret-key-at-least-32-bytes!", 24);
        let user_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();

        let token = auth.create_jwt(user_id, device_id).unwrap();
        let claims = auth.validate_jwt(&token).unwrap();

        assert_eq!(claims.user_id, user_id.to_string());
        assert_eq!(claims.device_id, device_id.to_string());
    }

    #[test]
    fn test_invalid_jwt() {
        let auth = AuthService::new(b"test-secret-key-at-least-32-bytes!", 24);
        let result = auth.validate_jwt("invalid-token");
        assert!(result.is_err());
    }

    #[test]
    fn test_pairing_token_generation() {
        let token = AuthService::generate_pairing_token();
        assert_eq!(token.len(), 64);
    }
}
