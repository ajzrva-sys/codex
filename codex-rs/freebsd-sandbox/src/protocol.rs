use codex_protocol::models::PermissionProfile;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;

pub(crate) const VERSION: u32 = 1;
pub(crate) const MAX_FRAME: usize = 1024 * 1024;
pub(crate) const CHUNK: usize = 16 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Launch {
    pub version: u32,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub policy_cwd: PathBuf,
    pub permissions: PermissionProfile,
    pub env: BTreeMap<String, String>,
    pub terminal: Option<TerminalSize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Request {
    Probe { version: u32 },
    Launch(Box<Launch>),
    Input { data: Vec<u8> },
    Eof,
    Resize { size: TerminalSize },
    Signal { signal: i32 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Response {
    Status { version: u32, description: String },
    Ready,
    Output { stderr: bool, data: Vec<u8> },
    Exit { code: i32 },
    Error { message: String },
}

pub(crate) fn send(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sandbox frame too large",
        ));
    }
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)
}

pub(crate) fn receive<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sandbox frame too large",
        ));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
