use alloc::string::String;
use core::borrow::Borrow;
use core::fmt;

/// A validated RPC group name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RpcGroup(String);

impl RpcGroup {
    pub(crate) fn from_validated(value: &str) -> Self {
        Self(value.into())
    }
}

impl TryFrom<&str> for RpcGroup {
    type Error = RpcGroupError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if !is_valid_segment(value) {
            return Err(RpcGroupError::InvalidFormat(value.into()));
        }
        Ok(Self(value.into()))
    }
}

impl fmt::Display for RpcGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for RpcGroup {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A validated RPC address in `group.method` form.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RpcAddress(String);

impl RpcAddress {
    pub(crate) fn validate(value: &str) -> Result<(), RpcAddressError> {
        let mut segments = value.split('.');
        let group = segments.next();
        let method = segments.next();
        if segments.next().is_some()
            || !group.is_some_and(is_valid_segment)
            || !method.is_some_and(is_valid_segment)
        {
            return Err(RpcAddressError::InvalidFormat(value.into()));
        }
        Ok(())
    }

    pub(crate) fn group(&self) -> &str {
        self.0.split_once('.').map_or("", |(group, _method)| group)
    }
}

impl TryFrom<&str> for RpcAddress {
    type Error = RpcAddressError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::validate(value)?;
        Ok(Self(value.into()))
    }
}

fn is_valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

impl Borrow<str> for RpcAddress {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RpcAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for RpcAddress {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Failure returned while parsing an [`RpcGroup`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RpcGroupError {
    /// The value is not a valid RPC group identifier.
    #[error("invalid RPC group: {0}")]
    InvalidFormat(String),
}

/// Failure returned while parsing an [`RpcAddress`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RpcAddressError {
    /// The value is not in validated `group.method` form.
    #[error("invalid RPC address: {0}")]
    InvalidFormat(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_group_and_method_identifiers() {
        let address = RpcAddress::try_from("scheduler.create-2");
        assert_eq!(
            address.as_ref().map(AsRef::as_ref),
            Ok("scheduler.create-2")
        );
    }

    #[test]
    fn parse_rejects_missing_or_extra_segments() {
        assert!(RpcAddress::try_from("scheduler").is_err());
        assert!(RpcAddress::try_from("scheduler.create.extra").is_err());
        assert!(RpcAddress::try_from("scheduler.").is_err());
    }

    #[test]
    fn group_accepts_one_identifier_segment() {
        let group = RpcGroup::try_from("device-control");
        assert_eq!(group.as_ref().map(AsRef::as_ref), Ok("device-control"));
    }

    #[test]
    fn group_rejects_empty_or_structured_values() {
        assert!(RpcGroup::try_from("").is_err());
        assert!(RpcGroup::try_from("device.status").is_err());
        assert!(RpcGroup::try_from("device/status").is_err());
    }
}
