//! The consumer's side of the handshake: whether a producer's `Hello` may
//! join, and if not, the `Reject` that says why.
//!
//! The rest of the consumer (importing, showing, releasing, pacing) depends
//! on a renderer and lives with it; Lumen's is the `lumen-surface` module.

use crate::wire::{AdapterId, Api, Hello, MAJOR, Reject, RejectReason};

/// What a consumer expects of a producer on one channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    /// The secret the endpoint string carried.
    pub token: [u8; 16],
    /// The channel.
    pub channel: String,
    /// The API the consumer imports with, or `None` when it has no GPU to
    /// import into.
    pub api: Option<Api>,
    /// The consumer's GPU.
    pub adapter: AdapterId,
}

/// Check `hello` against `expected`: `Ok` to welcome the producer, or the
/// rejection to send it. The token is compared in constant time.
pub fn check_hello(hello: &Hello, expected: &Expected) -> Result<(), Reject> {
    let reject = |reason: RejectReason, detail: String| Reject {
        reason,
        api: expected.api.unwrap_or(hello.api),
        adapter: expected.adapter,
        detail,
    };
    let differ = hello
        .token
        .iter()
        .zip(&expected.token)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if differ != 0 {
        return Err(reject(
            RejectReason::Token,
            "the token does not match".into(),
        ));
    }
    if hello.major != MAJOR {
        return Err(reject(
            RejectReason::Version,
            format!(
                "the producer speaks version {}, the consumer {MAJOR}",
                hello.major
            ),
        ));
    }
    if hello.channel != expected.channel {
        return Err(reject(
            RejectReason::Channel,
            format!("this endpoint is for channel `{}`", expected.channel),
        ));
    }
    let Some(api) = expected.api else {
        return Err(reject(
            RejectReason::Unsupported,
            "the consumer renders without a GPU and cannot show shared frames".into(),
        ));
    };
    if hello.api != api {
        return Err(reject(
            RejectReason::Api,
            format!("the consumer imports {api:?} images, not {:?}", hello.api),
        ));
    }
    if !hello.adapter.same_device(&expected.adapter) {
        return Err(reject(
            RejectReason::Adapter,
            "the producer renders on another GPU or driver than the consumer".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::MINOR;

    fn expected() -> Expected {
        Expected {
            token: [5; 16],
            channel: "scene".into(),
            api: Some(Api::Vulkan),
            adapter: AdapterId {
                device_uuid: [1; 16],
                driver_uuid: [2; 16],
                ..AdapterId::default()
            },
        }
    }

    fn hello() -> Hello {
        Hello {
            major: MAJOR,
            minor: MINOR,
            api: Api::Vulkan,
            pid: 1,
            caps: 0,
            token: [5; 16],
            adapter: expected().adapter,
            channel: "scene".into(),
            engine: "test".into(),
        }
    }

    /// A matching hello is welcome; each mismatch is refused with its
    /// reason, and the rejection names the consumer's GPU.
    #[test]
    fn a_hello_is_checked_field_by_field() {
        assert_eq!(check_hello(&hello(), &expected()), Ok(()));
        let reason = |h: Hello, e: Expected| check_hello(&h, &e).unwrap_err().reason;
        assert_eq!(
            reason(
                Hello {
                    token: [6; 16],
                    ..hello()
                },
                expected()
            ),
            RejectReason::Token
        );
        assert_eq!(
            reason(
                Hello {
                    major: MAJOR + 1,
                    ..hello()
                },
                expected()
            ),
            RejectReason::Version
        );
        assert_eq!(
            reason(
                Hello {
                    channel: "game".into(),
                    ..hello()
                },
                expected()
            ),
            RejectReason::Channel
        );
        assert_eq!(
            reason(
                Hello {
                    api: Api::Metal,
                    ..hello()
                },
                expected()
            ),
            RejectReason::Api
        );
        let mut other = hello();
        other.adapter.device_uuid = [9; 16];
        let rejected = check_hello(&other, &expected()).unwrap_err();
        assert_eq!(rejected.reason, RejectReason::Adapter);
        assert_eq!(rejected.adapter, expected().adapter);
        assert_eq!(
            reason(
                hello(),
                Expected {
                    api: None,
                    ..expected()
                }
            ),
            RejectReason::Unsupported
        );
    }
}
