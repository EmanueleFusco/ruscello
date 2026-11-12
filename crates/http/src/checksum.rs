use std::{
    io::{self, Read},
    str::FromStr,
};

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

/// Digest algorithms accepted for transfer integrity checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Crc32,
}

impl ChecksumAlgorithm {
    fn size(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Crc32 => 4,
        }
    }

    fn id(self) -> u8 {
        match self {
            Self::Md5 => 1,
            Self::Sha1 => 2,
            Self::Sha256 => 3,
            Self::Crc32 => 4,
        }
    }
}

impl FromStr for ChecksumAlgorithm {
    type Err = ChecksumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "md5" => Ok(Self::Md5),
            "sha1" | "sha-1" => Ok(Self::Sha1),
            "sha256" | "sha-256" => Ok(Self::Sha256),
            "crc32" => Ok(Self::Crc32),
            _ => Err(ChecksumError::Algorithm),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChecksumError {
    #[error("unknown checksum algorithm")]
    Algorithm,
    #[error("checksum has the wrong length or contains non-hexadecimal digits")]
    InvalidHex,
}

/// Expected digest provided by the user or a resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedChecksum {
    algorithm: ChecksumAlgorithm,
    value: Vec<u8>,
}

impl ExpectedChecksum {
    pub fn from_hex(algorithm: ChecksumAlgorithm, hex: &str) -> Result<Self, ChecksumError> {
        if hex.len() != algorithm.size() * 2 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ChecksumError::InvalidHex);
        }
        let value = (0..hex.len())
            .step_by(2)
            .map(|offset| {
                u8::from_str_radix(&hex[offset..offset + 2], 16)
                    .map_err(|_| ChecksumError::InvalidHex)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { algorithm, value })
    }

    pub(crate) fn bind_identity(&self, identity: [u8; 32]) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(identity);
        digest.update([self.algorithm.id()]);
        digest.update(&self.value);
        digest.finalize().into()
    }

    pub(crate) fn verify(
        &self,
        reader: impl Read,
        cancellation: &CancellationToken,
    ) -> io::Result<bool> {
        let expected = self.value.as_slice();
        match self.algorithm {
            ChecksumAlgorithm::Md5 => verify_digest::<Md5>(reader, expected, cancellation),
            ChecksumAlgorithm::Sha1 => verify_digest::<Sha1>(reader, expected, cancellation),
            ChecksumAlgorithm::Sha256 => verify_digest::<Sha256>(reader, expected, cancellation),
            ChecksumAlgorithm::Crc32 => {
                let mut digest = crc32fast::Hasher::new();
                feed(reader, |bytes| digest.update(bytes), cancellation)?;
                Ok(digest.finalize().to_be_bytes().as_slice() == expected)
            }
        }
    }
}

fn verify_digest<D: Digest>(
    reader: impl Read,
    expected: &[u8],
    cancellation: &CancellationToken,
) -> io::Result<bool> {
    let mut digest = D::new();
    feed(reader, |bytes| digest.update(bytes), cancellation)?;
    Ok(digest.finalize().as_slice() == expected)
}

fn feed(
    mut reader: impl Read,
    mut update: impl FnMut(&[u8]),
    cancellation: &CancellationToken,
) -> io::Result<()> {
    let mut buffer = vec![0; 128 * 1024];
    loop {
        if cancellation.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => update(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_standard_test_vectors() -> Result<(), Box<dyn std::error::Error>> {
        let data = b"123456789".as_slice();
        for (algorithm, expected) in [
            (ChecksumAlgorithm::Md5, "25f9e794323b453885f5181f1b624d0b"),
            (
                ChecksumAlgorithm::Sha1,
                "f7c3bc1d808e04732adf679965ccc34ca7ae3441",
            ),
            (
                ChecksumAlgorithm::Sha256,
                "15e2b0d3c33891ebb0f1ef609ec419420c20e320ce94c65fbc8c3312448eb225",
            ),
            (ChecksumAlgorithm::Crc32, "CBF43926"),
        ] {
            let expected = ExpectedChecksum::from_hex(algorithm, expected)?;
            assert!(expected.verify(data, &CancellationToken::new())?);
            assert!(!expected.verify(b"different".as_slice(), &CancellationToken::new())?);
        }
        Ok(())
    }

    #[test]
    fn rejects_invalid_hex_and_lengths() {
        assert_eq!(
            ExpectedChecksum::from_hex(ChecksumAlgorithm::Crc32, "gg000000"),
            Err(ChecksumError::InvalidHex)
        );
        assert_eq!(
            ExpectedChecksum::from_hex(ChecksumAlgorithm::Crc32, "1234"),
            Err(ChecksumError::InvalidHex)
        );
    }
}
