use std::{
    fs::File,
    io::{Read, Result},
    path::Path,
};

use sha2::{Digest, Sha256};

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];

    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_bytes() {
        assert_eq!(
            sha256_bytes(b"dotr"),
            "5daf20ab83e282988563c25b21a9ee60d56ec8aed6f9d0a2cdd9b0962e409237"
        );
    }
}
