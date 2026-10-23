//! The endpoint string: everything a producer needs to find and join a
//! surface, in one line it can take from an environment variable.
//!
//! ```text
//! v1;transport=unix:/run/user/1000/lumen-surface-1000/4242-9f3c0a1b2c3d.sock;channel=scene;token=4be1...;adapter=vk:<device uuid>:<driver uuid>;adapter-name=AMD Radeon RX 6800 XT
//! ```
//!
//! Fields are `key=value`, separated by `;`, after a leading version. A
//! value escapes `%`, `;` and `=` as `%25`, `%3B` and `%3D`. Unknown keys are
//! skipped, so a later version can add fields.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

/// The environment variable a producer reads its endpoint from by
/// convention.
pub const ENV: &str = "LUMEN_SURFACE";

/// How the producer reaches the consumer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// A Unix stream socket at this path (Linux, macOS).
    Unix(PathBuf),
}

/// The GPU the consumer renders on, which the producer should pick before
/// it makes its device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterHint {
    /// A Vulkan device: its `deviceUUID` and `driverUUID`.
    Vulkan {
        /// `VkPhysicalDeviceIDProperties::deviceUUID`.
        device_uuid: [u8; 16],
        /// `VkPhysicalDeviceIDProperties::driverUUID`.
        driver_uuid: [u8; 16],
    },
}

/// A parsed endpoint string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// Where to connect.
    pub transport: Transport,
    /// The surface's channel.
    pub channel: String,
    /// The secret the producer's `Hello` must carry.
    pub token: [u8; 16],
    /// The consumer's GPU, when it knows it.
    pub adapter: Option<AdapterHint>,
    /// The consumer's GPU by name, for a person or an adapter-name setting.
    pub adapter_name: Option<String>,
}

/// An endpoint string that does not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointError(pub String);

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad surface endpoint: {}", self.0)
    }
}

impl std::error::Error for EndpointError {}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '%' => out.push_str("%25"),
            ';' => out.push_str("%3B"),
            '=' => out.push_str("%3D"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(value: &str) -> Result<String, EndpointError> {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value
                .get(i + 1..i + 3)
                .ok_or_else(|| EndpointError(format!("a cut-off escape in `{value}`")))?;
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|_| EndpointError(format!("a bad escape in `{value}`")))?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| EndpointError("a value is not UTF-8".into()))
}

/// `bytes` as lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `N` bytes a hex string spells.
pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("v1")?;
        match &self.transport {
            Transport::Unix(path) => {
                write!(f, ";transport=unix:{}", escape(&path.to_string_lossy()))?
            }
        }
        write!(f, ";channel={}", escape(&self.channel))?;
        write!(f, ";token={}", hex(&self.token))?;
        if let Some(AdapterHint::Vulkan {
            device_uuid,
            driver_uuid,
        }) = &self.adapter
        {
            write!(f, ";adapter=vk:{}:{}", hex(device_uuid), hex(driver_uuid))?;
        }
        if let Some(name) = &self.adapter_name {
            write!(f, ";adapter-name={}", escape(name))?;
        }
        Ok(())
    }
}

impl FromStr for Endpoint {
    type Err = EndpointError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut fields = s.trim().split(';');
        match fields.next() {
            Some("v1") => {}
            Some(other) => {
                return Err(EndpointError(format!(
                    "version `{other}`; this producer speaks v1"
                )));
            }
            None => return Err(EndpointError("empty".into())),
        }
        let (mut transport, mut channel, mut token) = (None, None, None);
        let (mut adapter, mut adapter_name) = (None, None);
        for field in fields.filter(|f| !f.is_empty()) {
            let (key, value) = field
                .split_once('=')
                .ok_or_else(|| EndpointError(format!("`{field}` has no `=`")))?;
            match key {
                "transport" => {
                    let value = unescape(value)?;
                    match value.split_once(':') {
                        Some(("unix", path)) => {
                            transport = Some(Transport::Unix(PathBuf::from(path)));
                        }
                        _ => {
                            return Err(EndpointError(format!(
                                "transport `{value}` is not one this build speaks"
                            )));
                        }
                    }
                }
                "channel" => channel = Some(unescape(value)?),
                "token" => {
                    token =
                        Some(unhex::<16>(value).ok_or_else(|| {
                            EndpointError("the token is not 32 hex digits".into())
                        })?);
                }
                "adapter" => {
                    let mut parts = value.split(':');
                    adapter = match (parts.next(), parts.next(), parts.next()) {
                        (Some("vk"), Some(device), Some(driver)) => Some(AdapterHint::Vulkan {
                            device_uuid: unhex(device)
                                .ok_or_else(|| EndpointError("a bad device uuid".into()))?,
                            driver_uuid: unhex(driver)
                                .ok_or_else(|| EndpointError("a bad driver uuid".into()))?,
                        }),
                        // An adapter in a form this build does not read is a
                        // hint it cannot use, not an error.
                        _ => None,
                    };
                }
                "adapter-name" => adapter_name = Some(unescape(value)?),
                _ => {}
            }
        }
        Ok(Endpoint {
            transport: transport.ok_or_else(|| EndpointError("no transport".into()))?,
            channel: channel.ok_or_else(|| EndpointError("no channel".into()))?,
            token: token.ok_or_else(|| EndpointError("no token".into()))?,
            adapter,
            adapter_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint {
            transport: Transport::Unix("/run/user/1000/lumen-surface/a;b.sock".into()),
            channel: "scene=1".into(),
            token: [0xab; 16],
            adapter: Some(AdapterHint::Vulkan {
                device_uuid: [1; 16],
                driver_uuid: [2; 16],
            }),
            adapter_name: Some("AMD Radeon; 100%".into()),
        }
    }

    /// An endpoint survives the string, escapes and all.
    #[test]
    fn endpoints_round_trip() {
        let e = endpoint();
        let s = e.to_string();
        assert!(s.starts_with("v1;transport=unix:/run/user/1000/lumen-surface/a%3Bb.sock;"));
        assert_eq!(s.parse::<Endpoint>(), Ok(e));
    }

    /// Missing fields and other versions are refused; unknown keys and an
    /// adapter form this build cannot read are not.
    #[test]
    fn endpoints_are_checked() {
        assert!("v2;transport=unix:/x".parse::<Endpoint>().is_err());
        assert!("v1;channel=a".parse::<Endpoint>().is_err());
        let token = hex(&[7; 16]);
        let e: Endpoint =
            format!("v1;transport=unix:/x;channel=a;token={token};future=1;adapter=dx:1")
                .parse()
                .expect("parses");
        assert_eq!(e.adapter, None);
        assert_eq!(e.token, [7; 16]);
    }
}
