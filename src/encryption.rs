use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use age::{Decryptor, Encryptor};
use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

use crate::environment::Environment;

pub fn load_recipients(path: &Path) -> Result<Vec<age::x25519::Recipient>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read recipients file {}", path.display()))?;
    parse_recipients(&raw)
}

pub fn load_identities(path: &Path) -> Result<Vec<age::x25519::Identity>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read identity file {}", path.display()))?;
    parse_identities(&raw)
}

pub fn encrypt_bytes(plaintext: &[u8], recipients: &[age::x25519::Recipient]) -> Result<Vec<u8>> {
    if recipients.is_empty() {
        bail!("age encryption requires at least one recipient");
    }

    let recipient_refs = recipients
        .iter()
        .map(|recipient| recipient as &dyn age::Recipient);
    let encryptor =
        Encryptor::with_recipients(recipient_refs).context("failed to create age encryptor")?;

    let mut encrypted = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut encrypted)
        .context("failed to create age encryptor")?;
    writer
        .write_all(plaintext)
        .context("failed to encrypt bytes with age")?;
    writer.finish().context("failed to finish age encryption")?;

    Ok(encrypted)
}

pub fn decrypt_bytes(ciphertext: &[u8], identities: &[age::x25519::Identity]) -> Result<Vec<u8>> {
    if identities.is_empty() {
        bail!("age decryption requires at least one identity");
    }

    let decryptor = Decryptor::new(ciphertext).context("failed to parse age ciphertext")?;
    let identity_refs = identities
        .iter()
        .map(|identity| identity as &dyn age::Identity);
    let mut reader = decryptor
        .decrypt(identity_refs)
        .context("failed to decrypt age ciphertext")?;
    let mut plaintext = Vec::new();
    reader
        .read_to_end(&mut plaintext)
        .context("failed to read decrypted age plaintext")?;
    Ok(plaintext)
}

pub fn parse_recipients(raw: &str) -> Result<Vec<age::x25519::Recipient>> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            age::x25519::Recipient::from_str(line)
                .map_err(|err| anyhow::anyhow!("invalid age recipient {line}: {err}"))
        })
        .collect()
}

pub fn parse_identities(raw: &str) -> Result<Vec<age::x25519::Identity>> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            age::x25519::Identity::from_str(line)
                .map_err(|err| anyhow::anyhow!("invalid age identity in identity file: {err}"))
        })
        .collect()
}

pub fn resolve_recipients_file(repo_root: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    }
}

pub fn resolve_identity_file(env: &Environment, raw: &str) -> PathBuf {
    env.expand_tilde(raw)
}

pub fn find_age_secret(repo_root: &Path) -> Result<Option<PathBuf>> {
    for entry in WalkDir::new(repo_root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let bytes = fs::read(entry.path())
            .with_context(|| format!("failed to scan {}", entry.path().display()))?;
        if bytes
            .windows(b"AGE-SECRET-KEY-1".len())
            .any(|window| window == b"AGE-SECRET-KEY-1")
        {
            return Ok(Some(entry.path().to_path_buf()));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn encrypts_and_decrypts_with_age_identity() {
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();

        let encrypted = encrypt_bytes(b"secret-token", &[recipient]).unwrap();
        assert!(
            !encrypted
                .windows(b"secret-token".len())
                .any(|w| w == b"secret-token")
        );

        let decrypted = decrypt_bytes(&encrypted, &[identity]).unwrap();
        assert_eq!(decrypted, b"secret-token");
    }

    #[test]
    fn detects_age_secret_material_in_repo() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("backup")).unwrap();
        fs::write(
            dir.path().join("oops.txt"),
            "AGE-SECRET-KEY-1THISSHOULDNOTBEINGIT",
        )
        .unwrap();

        let found = find_age_secret(dir.path()).unwrap();
        assert_eq!(found, Some(dir.path().join("oops.txt")));
    }
}
