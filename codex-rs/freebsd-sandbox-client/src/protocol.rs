use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;

pub const VERSION: u32 = 1;
pub const MAX_FRAME: usize = 1024 * 1024;
pub const CHUNK: usize = 16 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub version: u32,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub policy_cwd: PathBuf,
    pub permissions: serde_json::Value,
    pub env: BTreeMap<String, String>,
    pub terminal: Option<TerminalSize>,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Launch")
            .field("version", &self.version)
            .field("argument_count", &self.argv.len())
            .field("environment_count", &self.env.len())
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Request {
    Probe { version: u32 },
    Capabilities { version: u32 },
    Launch(Box<Launch>),
    Input { data: Vec<u8> },
    Eof,
    Resize { size: TerminalSize },
    Signal { signal: i32 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Response {
    Status { version: u32, description: String },
    Capabilities(Capabilities),
    Ready,
    Output { stderr: bool, data: Vec<u8> },
    Exit { code: i32 },
    Error { message: String },
}

pub fn send(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
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

pub fn receive<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub version: u32,
    pub service: String,
    pub features: Vec<String>,
}
