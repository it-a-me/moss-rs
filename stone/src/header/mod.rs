// SPDX-FileCopyrightText: Copyright © 2020-2023 Serpent OS Developers
//
// SPDX-License-Identifier: MPL-2.0

use std::io::{self, Read};

use thiserror::Error;

use crate::ReadExt;

pub mod v1;

/// Well defined magic field for a stone header
pub const STONE_MAGIC: u32 = 0x006d6f73;

/// Format versions are defined as u32, to allow further mangling in future
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V1 = 1,
}

/// The stone format uses an agnostic approach requiring a valid magic field
/// in the first 4 bytes, and a version specifier in the last 4 bytes, using
/// big endian order.
///
/// When the version is decoded, we can create the appropriate, version-specific
/// reader implementation, ensuring the container format is extensible well into
/// the future
#[repr(C)]
pub struct AgnosticHeader {
    /// 4-bytes, BE (u32): Magic to quickly identify a stone file
    pub magic: [u8; 4],

    /// 24 bytes, version specific
    pub data: [u8; 24],

    /// 4-bytes, BE (u32): Format version used in the container
    pub version: [u8; 4],
}

impl AgnosticHeader {
    fn decode<R: Read>(mut reader: R) -> Result<Self, io::Error> {
        let magic = reader.read_array()?;
        let data = reader.read_array()?;
        let version = reader.read_array()?;

        Ok(Self {
            magic,
            data,
            version,
        })
    }
}

pub enum Header {
    V1(v1::Header),
}

impl Header {
    pub fn version(&self) -> Version {
        match self {
            Header::V1(_) => Version::V1,
        }
    }

    pub fn encode(&self) -> AgnosticHeader {
        let magic = u32::to_be_bytes(STONE_MAGIC);
        let version = u32::to_be_bytes(self.version() as u32);

        let data = match self {
            Header::V1(v1) => v1.encode(),
        };

        AgnosticHeader {
            magic,
            data,
            version,
        }
    }

    pub fn decode<R: Read>(reader: R) -> Result<Self, DecodeError> {
        let header = AgnosticHeader::decode(reader)?;

        if STONE_MAGIC != u32::from_be_bytes(header.magic) {
            return Err(DecodeError::InvalidMagic);
        }

        let version = match u32::from_be_bytes(header.version) {
            1 => Version::V1,
            v => return Err(DecodeError::UnknownVersion(v)),
        };

        Ok(match version {
            Version::V1 => Self::V1(v1::Header::decode(header.data)?),
        })
    }
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("Header must be {} bytes long", std::mem::size_of::<AgnosticHeader>())]
    NotEnoughBytes,
    #[error("Invalid magic")]
    InvalidMagic,
    #[error("Unknown version: {0}")]
    UnknownVersion(u32),
    #[error("v1 error: {0}")]
    V1(#[from] v1::DecodeError),
    #[error("io error: {0}")]
    Io(io::Error),
}

impl From<io::Error> for DecodeError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::UnexpectedEof => DecodeError::NotEnoughBytes,
            _ => DecodeError::Io(error),
        }
    }
}