use std::path::Path;

use rcgen::*;

/// A generated certificate and its private key in PEM format.
pub struct KeyPairPem {
    pub cert_pem: String,
    pub key_pem: String,
}

/// Generate a self-signed CA certificate and key.
pub fn generate_ca(common_name: &str) -> Result<KeyPairPem, anyhow::Error> {
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec![common_name.to_string()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let cert = params.self_signed(&key)?;
    Ok(KeyPairPem {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

/// Helper: load a CA certificate and key pair, returning a `Certificate` object
/// suitable for signing new certificates via `signed_by()`.
fn load_ca(ca_cert_pem: &str, ca_key_pem: &str) -> Result<(KeyPair, Certificate), anyhow::Error> {
    let ca_key = KeyPair::from_pem(ca_key_pem)
        .map_err(|e| anyhow::anyhow!("Failed to parse CA key: {}", e))?;
    let ca_params = CertificateParams::from_ca_cert_pem(ca_cert_pem)
        .map_err(|e| anyhow::anyhow!("Failed to parse CA cert: {}", e))?;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .map_err(|e| anyhow::anyhow!("Failed to reconstruct CA cert: {}", e))?;
    Ok((ca_key, ca_cert))
}

/// Generate a server certificate signed by the given CA.
/// `sans` includes DNS names (e.g. ["heliondb.local", "localhost"]).
pub fn generate_server(
    cn: &str,
    sans: &[String],
    ca_cert_pem: &str,
    ca_key_pem: &str,
) -> Result<KeyPairPem, anyhow::Error> {
    let (ca_key, ca_cert) = load_ca(ca_cert_pem, ca_key_pem)?;

    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(sans.to_vec())?;
    params.distinguished_name.push(DnType::CommonName, cn);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    let cert = params.signed_by(&key, &ca_cert, &ca_key)?;
    Ok(KeyPairPem {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

/// Generate a client certificate signed by the given CA.
/// The Common Name (CN) will be used as the database username.
pub fn generate_client(
    cn: &str,
    ca_cert_pem: &str,
    ca_key_pem: &str,
) -> Result<KeyPairPem, anyhow::Error> {
    if cn.len() > 255 {
        anyhow::bail!("Common Name too long ({} chars, max 255)", cn.len());
    }
    if cn.is_empty() {
        anyhow::bail!("Common Name cannot be empty");
    }

    let (ca_key, ca_cert) = load_ca(ca_cert_pem, ca_key_pem)?;

    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec![cn.to_string()])?;
    params.distinguished_name.push(DnType::CommonName, cn);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    let cert = params.signed_by(&key, &ca_cert, &ca_key)?;
    Ok(KeyPairPem {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

/// Write a key pair to disk with secure permissions.
/// The cert file gets 0644, the key file gets 0600 (owner read-only).
pub fn write_key_pair(
    cert_path: &Path,
    key_path: &Path,
    kp: &KeyPairPem,
) -> Result<(), anyhow::Error> {
    std::fs::write(cert_path, kp.cert_pem.as_bytes())?;
    std::fs::write(key_path, kp.key_pem.as_bytes())?;

    // Set secure permissions on Unix: key file owner-read-only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}
