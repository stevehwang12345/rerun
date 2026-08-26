//! Verification for the signed RMS Edge Agent mDNS advertisement contract.

use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine as _, prelude::BASE64_URL_SAFE_NO_PAD};
use ring::{hmac, signature};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

const SERVICE_TYPE: &str = "_rms._tcp.local";
const MAX_CLOCK_SKEW_SECONDS: i64 = 120;
const MAX_NAME_LEN: usize = 64;
const MAX_PATH_LEN: usize = 256;
const MAX_ORG_KEY_ID_LEN: usize = 32;
const MAX_ORGANIZATION_ID_LEN: usize = 64;
const PUBLIC_KEY_LEN: usize = 32;
const NONCE_LEN: usize = 16;
const SIGNATURE_LEN: usize = 64;
const HMAC_LEN: usize = 32;

const REQUIRED_FIELDS: [&str; 10] = [
    "caps", "id", "kind", "name", "nonce", "path", "pk", "sig", "ts", "v",
];
const OPTIONAL_FIELDS: [&str; 3] = ["org", "org_kid", "org_proof"];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum EdgeCapability {
    Mavlink,
    Ros2Dds,
    Status,
}

impl EdgeCapability {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "mavlink" => Some(Self::Mavlink),
            "ros2_dds" => Some(Self::Ros2Dds),
            "status" => Some(Self::Status),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EdgeOrganizationProof<'a> {
    pub(crate) organization_id: &'a str,
    pub(crate) key_id: &'a str,
    pub(crate) secret: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedEdgeAdvertisement {
    pub(crate) device_id: String,
    pub(crate) public_key: [u8; PUBLIC_KEY_LEN],
    pub(crate) nonce: [u8; NONCE_LEN],
    pub(crate) timestamp_seconds: i64,
    pub(crate) display_name: String,
    pub(crate) device_kind: &'static str,
    pub(crate) capabilities: Vec<EdgeCapability>,
    pub(crate) path: String,
    pub(crate) organization_id: Option<String>,
    pub(crate) organization_trusted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvalidEdgeAdvertisement;

pub(crate) fn verify(
    instance: &str,
    port: u16,
    properties: &BTreeMap<String, String>,
    now_seconds: i64,
    organization_proof: Option<EdgeOrganizationProof<'_>>,
) -> Result<VerifiedEdgeAdvertisement, InvalidEdgeAdvertisement> {
    if port == 0 || properties.len() > REQUIRED_FIELDS.len() + OPTIONAL_FIELDS.len() {
        return Err(InvalidEdgeAdvertisement);
    }
    if properties.keys().any(|key| {
        !REQUIRED_FIELDS.contains(&key.as_str()) && !OPTIONAL_FIELDS.contains(&key.as_str())
    }) || REQUIRED_FIELDS
        .iter()
        .any(|field| !properties.contains_key(*field))
    {
        return Err(InvalidEdgeAdvertisement);
    }
    if properties.get("v").map(String::as_str) != Some("1") {
        return Err(InvalidEdgeAdvertisement);
    }

    let device_id = required(properties, "id")?;
    let public_key = decode_exact::<PUBLIC_KEY_LEN>(required(properties, "pk")?)?;
    if derive_device_id(&public_key) != device_id {
        return Err(InvalidEdgeAdvertisement);
    }
    let expected_instance = format!("{device_id}.{SERVICE_TYPE}");
    if instance != expected_instance {
        return Err(InvalidEdgeAdvertisement);
    }

    let nonce = decode_exact::<NONCE_LEN>(required(properties, "nonce")?)?;
    let timestamp_seconds = required(properties, "ts")?
        .parse::<i64>()
        .map_err(|_error| InvalidEdgeAdvertisement)?;
    if timestamp_seconds.abs_diff(now_seconds) > MAX_CLOCK_SKEW_SECONDS as u64 {
        return Err(InvalidEdgeAdvertisement);
    }

    let display_name = required(properties, "name")?;
    if display_name.is_empty()
        || display_name.chars().count() > MAX_NAME_LEN
        || display_name.chars().any(char::is_control)
    {
        return Err(InvalidEdgeAdvertisement);
    }
    let device_kind = parse_device_kind(required(properties, "kind")?)?;
    let capabilities = parse_capabilities(required(properties, "caps")?)?;
    if !capabilities.iter().any(|capability| {
        matches!(
            capability,
            EdgeCapability::Mavlink | EdgeCapability::Ros2Dds
        )
    }) {
        return Err(InvalidEdgeAdvertisement);
    }
    let path = required(properties, "path")?;
    validate_path(path)?;

    let canonical = canonical_bytes(
        instance,
        device_id,
        required(properties, "nonce")?,
        timestamp_seconds,
        port,
        path,
        device_kind,
        required(properties, "caps")?,
        display_name,
    );
    let signature_bytes = decode_exact::<SIGNATURE_LEN>(required(properties, "sig")?)?;
    signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
        .verify(canonical.as_bytes(), &signature_bytes)
        .map_err(|_error| InvalidEdgeAdvertisement)?;

    let (organization_id, organization_trusted) =
        verify_organization_proof(properties, &canonical, organization_proof)?;

    Ok(VerifiedEdgeAdvertisement {
        device_id: device_id.to_owned(),
        public_key,
        nonce,
        timestamp_seconds,
        display_name: display_name.to_owned(),
        device_kind,
        capabilities,
        path: path.to_owned(),
        organization_id,
        organization_trusted,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the signed wire contract is intentionally represented field by field"
)]
pub(crate) fn canonical_bytes(
    instance: &str,
    device_id: &str,
    nonce: &str,
    timestamp_seconds: i64,
    port: u16,
    path: &str,
    device_kind: &str,
    capabilities: &str,
    display_name: &str,
) -> String {
    format!(
        "rms-advertisement-v1\nservice={SERVICE_TYPE}\ninstance={instance}\nid={device_id}\nnonce={nonce}\nts={timestamp_seconds}\nport={port}\npath={path}\nkind={device_kind}\ncaps={capabilities}\nname={display_name}"
    )
}

pub(crate) fn derive_device_id(public_key: &[u8; PUBLIC_KEY_LEN]) -> String {
    let digest = Sha256::digest(public_key);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).hyphenated().to_string()
}

fn verify_organization_proof(
    properties: &BTreeMap<String, String>,
    canonical: &str,
    configured: Option<EdgeOrganizationProof<'_>>,
) -> Result<(Option<String>, bool), InvalidEdgeAdvertisement> {
    let advertised_organization = properties.get("org");
    let advertised_key_id = properties.get("org_kid");
    let advertised_proof = properties.get("org_proof");
    if !(advertised_organization.is_some() == advertised_key_id.is_some()
        && advertised_key_id.is_some() == advertised_proof.is_some())
    {
        return Err(InvalidEdgeAdvertisement);
    }
    let (Some(organization_id), Some(key_id), Some(proof)) =
        (advertised_organization, advertised_key_id, advertised_proof)
    else {
        return Ok((None, false));
    };
    if !valid_organization_token(organization_id, MAX_ORGANIZATION_ID_LEN)
        || !valid_organization_token(key_id, MAX_ORG_KEY_ID_LEN)
    {
        return Err(InvalidEdgeAdvertisement);
    }
    let Some(configured) = configured else {
        return Ok((Some(organization_id.clone()), false));
    };
    if configured.organization_id != organization_id
        || configured.key_id != key_id
        || configured.secret.len() < 32
    {
        return Err(InvalidEdgeAdvertisement);
    }
    let proof = decode_exact::<HMAC_LEN>(proof)?;
    let message = format!("{canonical}\norg={organization_id}\norg_kid={key_id}");
    let key = hmac::Key::new(hmac::HMAC_SHA256, configured.secret);
    hmac::verify(&key, message.as_bytes(), &proof).map_err(|_error| InvalidEdgeAdvertisement)?;
    Ok((Some(organization_id.clone()), true))
}

fn valid_organization_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn parse_capabilities(value: &str) -> Result<Vec<EdgeCapability>, InvalidEdgeAdvertisement> {
    if value.is_empty() || value.len() > 64 {
        return Err(InvalidEdgeAdvertisement);
    }
    let parts = value.split(',').collect::<Vec<_>>();
    let unique = parts.iter().copied().collect::<BTreeSet<_>>();
    if parts.len() != unique.len() || !parts.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(InvalidEdgeAdvertisement);
    }
    parts
        .into_iter()
        .map(|part| EdgeCapability::parse(part).ok_or(InvalidEdgeAdvertisement))
        .collect()
}

fn parse_device_kind(value: &str) -> Result<&'static str, InvalidEdgeAdvertisement> {
    match value {
        "robot" => Ok("robot"),
        "drone" => Ok("drone"),
        "vehicle" => Ok("vehicle"),
        "camera" => Ok("camera"),
        "gateway" => Ok("gateway"),
        _ => Err(InvalidEdgeAdvertisement),
    }
}

fn validate_path(path: &str) -> Result<(), InvalidEdgeAdvertisement> {
    if path.is_empty()
        || path.len() > MAX_PATH_LEN
        || !path.starts_with('/')
        || path.contains("..")
        || path.contains(['?', '#'])
        || !path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~')
        })
    {
        return Err(InvalidEdgeAdvertisement);
    }
    Ok(())
}

fn required<'a>(
    properties: &'a BTreeMap<String, String>,
    field: &str,
) -> Result<&'a str, InvalidEdgeAdvertisement> {
    properties
        .get(field)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(InvalidEdgeAdvertisement)
}

fn decode_exact<const N: usize>(value: &str) -> Result<[u8; N], InvalidEdgeAdvertisement> {
    let decoded = BASE64_URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_error| InvalidEdgeAdvertisement)?;
    decoded
        .try_into()
        .map_err(|_error| InvalidEdgeAdvertisement)
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, prelude::BASE64_URL_SAFE_NO_PAD};
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    use super::*;

    fn signed_properties(seed: [u8; 32], now_seconds: i64) -> (String, BTreeMap<String, String>) {
        let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("test key is valid");
        let public_key: [u8; 32] = key_pair
            .public_key()
            .as_ref()
            .try_into()
            .expect("Ed25519 key has 32 bytes");
        let device_id = derive_device_id(&public_key);
        let instance = format!("{device_id}.{SERVICE_TYPE}");
        let nonce = BASE64_URL_SAFE_NO_PAD.encode([9_u8; 16]);
        let caps = "mavlink,ros2_dds,status";
        let canonical = canonical_bytes(
            &instance,
            &device_id,
            &nonce,
            now_seconds,
            9877,
            "/v1/edge",
            "robot",
            caps,
            "Robot 42",
        );
        let mut properties = BTreeMap::from([
            ("v".to_owned(), "1".to_owned()),
            ("id".to_owned(), device_id),
            ("pk".to_owned(), BASE64_URL_SAFE_NO_PAD.encode(public_key)),
            ("nonce".to_owned(), nonce),
            ("ts".to_owned(), now_seconds.to_string()),
            ("name".to_owned(), "Robot 42".to_owned()),
            ("kind".to_owned(), "robot".to_owned()),
            ("caps".to_owned(), caps.to_owned()),
            ("path".to_owned(), "/v1/edge".to_owned()),
            (
                "sig".to_owned(),
                BASE64_URL_SAFE_NO_PAD.encode(key_pair.sign(canonical.as_bytes()).as_ref()),
            ),
        ]);
        let org_key = [3_u8; 32];
        let org_message = format!("{canonical}\norg=org-rms\norg_kid=fleet-a");
        let org_proof = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, &org_key),
            org_message.as_bytes(),
        );
        properties.insert("org".to_owned(), "org-rms".to_owned());
        properties.insert("org_kid".to_owned(), "fleet-a".to_owned());
        properties.insert(
            "org_proof".to_owned(),
            BASE64_URL_SAFE_NO_PAD.encode(org_proof.as_ref()),
        );
        (instance, properties)
    }

    #[test]
    fn verifies_signed_and_organization_authenticated_advertisement() {
        let now = 1_800_000_000;
        let (instance, properties) = signed_properties([7_u8; 32], now);
        let verified = verify(
            &instance,
            9877,
            &properties,
            now,
            Some(EdgeOrganizationProof {
                organization_id: "org-rms",
                key_id: "fleet-a",
                secret: &[3_u8; 32],
            }),
        )
        .expect("valid advertisement verifies");
        assert!(verified.organization_trusted);
        assert_eq!(verified.organization_id.as_deref(), Some("org-rms"));
        assert_eq!(verified.device_kind, "robot");
        assert_eq!(verified.capabilities.len(), 3);

        assert_eq!(
            verify(
                &instance,
                9877,
                &properties,
                now,
                Some(EdgeOrganizationProof {
                    organization_id: "org-other",
                    key_id: "fleet-a",
                    secret: &[3_u8; 32],
                }),
            ),
            Err(InvalidEdgeAdvertisement),
        );
    }

    #[test]
    fn rejects_tampering_stale_packets_and_public_key_id_mismatch() {
        let now = 1_800_000_000;
        let (instance, mut properties) = signed_properties([8_u8; 32], now);
        properties.insert("kind".to_owned(), "drone".to_owned());
        assert_eq!(
            verify(&instance, 9877, &properties, now, None),
            Err(InvalidEdgeAdvertisement)
        );

        let (instance, properties) = signed_properties([8_u8; 32], now);
        assert_eq!(
            verify(&instance, 9877, &properties, now + 121, None),
            Err(InvalidEdgeAdvertisement)
        );

        let mut properties = properties;
        properties.insert("id".to_owned(), Uuid::nil().to_string());
        assert_eq!(
            verify(&instance, 9877, &properties, now, None),
            Err(InvalidEdgeAdvertisement)
        );
    }
}
