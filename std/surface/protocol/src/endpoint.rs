//! The endpoint string: everything a producer needs to find and join a
//! surface, in one line it can take from an environment variable.
//!
//! ```text
//! v1;transport=unix:/run/user/1000/lumen-surface-1000/4242-9f3c0a1b2c3d.sock;channel=scene;token=4be1...;adapter=vk:<device uuid>:<driver uuid>;adapter-name=AMD Radeon RX 6800 XT
//! v1;transport=pipe:\\.\pipe\lumen-surface-4242-9f3c0a1b2c3d;channel=scene;token=4be1...;adapter=dx:<luid>
//! v1;transport=unix:/var/folders/.../4242-9f3c0a1b2c3d.sock;mach=dev.lumenfx.surface.4242-9f3c0a1b2c3d;channel=scene;token=4be1...;adapter=mtl:<registry id>
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
    /// A Unix stream socket at this path, OS handles passed as file
    /// descriptors with the frames (Linux).
    Unix(PathBuf),
    /// A Unix stream socket at `path` for the messages, and the Mach
    /// bootstrap service `service` the producer sends its IOSurface ports to
    /// (macOS). The endpoint string spells it `transport=unix:<path>` and
    /// `mach=<service>`.
    UnixMach {
        /// The socket.
        path: PathBuf,
        /// The bootstrap service name.
        service: String,
    },
    /// A named pipe, `\\.\pipe\...`; the consumer copies each handle out of
    /// the producer's process (Windows).
    Pipe(String),
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
    /// A Direct3D 12 adapter: its LUID, `LowPart` then `HighPart`, each
    /// little-endian (`dx:<16 hex digits>`).
    D3d12 {
        /// `DXGI_ADAPTER_DESC1::AdapterLuid` as bytes.
        luid: [u8; 8],
    },
    /// A Metal device: its `registryID` (`mtl:<16 hex digits>`, the number
    /// most significant digit first).
    Metal {
        /// `MTLDevice.registryID`.
        registry_id: u64,
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
            Transport::UnixMach { path, service } => write!(
                f,
                ";transport=unix:{};mach={}",
                escape(&path.to_string_lossy()),
                escape(service)
            )?,
            Transport::Pipe(name) => write!(f, ";transport=pipe:{}", escape(name))?,
        }
        write!(f, ";channel={}", escape(&self.channel))?;
        write!(f, ";token={}", hex(&self.token))?;
        match &self.adapter {
            Some(AdapterHint::Vulkan {
                device_uuid,
                driver_uuid,
            }) => write!(f, ";adapter=vk:{}:{}", hex(device_uuid), hex(driver_uuid))?,
            Some(AdapterHint::D3d12 { luid }) => write!(f, ";adapter=dx:{}", hex(luid))?,
            Some(AdapterHint::Metal { registry_id }) => {
                write!(f, ";adapter=mtl:{registry_id:016x}")?
            }
            None => {}
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
        let (mut adapter, mut adapter_name, mut mach) = (None, None, None);
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
                        Some(("pipe", name)) => {
                            transport = Some(Transport::Pipe(name.to_string()));
                        }
                        _ => {
                            return Err(EndpointError(format!(
                                "transport `{value}` is not one this build speaks"
                            )));
                        }
                    }
                }
                "mach" => mach = Some(unescape(value)?),
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
                        (Some("dx"), Some(luid), None) => Some(AdapterHint::D3d12 {
                            luid: unhex(luid)
                                .ok_or_else(|| EndpointError("a bad adapter luid".into()))?,
                        }),
                        (Some("mtl"), Some(id), None) => Some(AdapterHint::Metal {
                            registry_id: (id.len() == 16)
                                .then(|| u64::from_str_radix(id, 16).ok())
                                .flatten()
                                .ok_or_else(|| EndpointError("a bad registry id".into()))?,
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
        let transport = match (transport, mach) {
            (Some(Transport::Unix(path)), Some(service)) => Transport::UnixMach { path, service },
            (Some(transport), _) => transport,
            (None, _) => return Err(EndpointError("no transport".into())),
        };
        Ok(Endpoint {
            transport,
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
            format!("v1;transport=unix:/x;channel=a;token={token};future=1;adapter=gl:1")
                .parse()
                .expect("parses");
        assert_eq!(e.adapter, None);
        assert_eq!(e.token, [7; 16]);
        assert!(
            format!("v1;transport=unix:/x;channel=a;token={token};adapter=dx:12")
                .parse::<Endpoint>()
                .is_err(),
            "a cut-off LUID"
        );
    }

    /// The Windows and macOS transports and adapter forms survive the
    /// string too.
    #[test]
    fn other_transports_round_trip() {
        let pipe = Endpoint {
            transport: Transport::Pipe(r"\\.\pipe\lumen-surface-1-ab".into()),
            adapter: Some(AdapterHint::D3d12 {
                luid: [1, 2, 3, 4, 5, 6, 7, 8],
            }),
            ..endpoint()
        };
        let s = pipe.to_string();
        assert!(
            s.contains(r";transport=pipe:\\.\pipe\lumen-surface-1-ab;"),
            "{s}"
        );
        assert!(s.contains(";adapter=dx:0102030405060708"), "{s}");
        assert_eq!(s.parse::<Endpoint>(), Ok(pipe));
        let mach = Endpoint {
            transport: Transport::UnixMach {
                path: "/tmp/lumen-surface-501/1-ab.sock".into(),
                service: "dev.lumenfx.surface.1-ab".into(),
            },
            adapter: Some(AdapterHint::Metal {
                registry_id: 0x1000_0000_0000_0abc,
            }),
            ..endpoint()
        };
        let s = mach.to_string();
        assert!(s.contains(";mach=dev.lumenfx.surface.1-ab;"), "{s}");
        assert!(s.contains(";adapter=mtl:1000000000000abc"), "{s}");
        assert_eq!(s.parse::<Endpoint>(), Ok(mach));
    }
}
