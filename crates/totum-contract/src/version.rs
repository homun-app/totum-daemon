//! Versioni delle superfici e regole di negoziazione.
//!
//! Regole verificabili:
//!
//! - una versione è compatibile soltanto con la stessa versione maggiore;
//!   una maggiore sconosciuta non è mai compatibile;
//! - una differenza minore è additiva: si sceglie la più alta in comune
//!   nell'intervallo dichiarato da entrambe le parti;
//! - un intervallo copre una sola versione maggiore, con minimo non superiore
//!   al massimo.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::ProtocolError;
use crate::limits::MAX_VERSION_COMPONENT;

/// Versione di una superficie, composta da maggiore e minore.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Version {
    major: u16,
    minor: u16,
}

impl Version {
    /// Costruisce una versione. La validità si controlla con
    /// [`Version::is_valid`] o in decodifica.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Versione maggiore.
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Versione minore.
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Vero se le componenti rientrano nei limiti di lavoro.
    pub const fn is_valid(self) -> bool {
        self.major >= 1
            && self.major <= MAX_VERSION_COMPONENT
            && self.minor <= MAX_VERSION_COMPONENT
    }

    /// La compatibilità richiede la stessa versione maggiore.
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVersion {
    major: u16,
    minor: u16,
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawVersion::deserialize(deserializer)?;
        let version = Version::new(raw.major, raw.minor);
        if !version.is_valid() {
            return Err(serde::de::Error::custom(
                "version components must be between 1.0 and 999.999",
            ));
        }
        Ok(version)
    }
}

/// Intervallo chiuso di versioni supportate, entro una sola maggiore.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolRange {
    min: Version,
    max: Version,
}

impl ProtocolRange {
    /// Costruisce un intervallo. La validità si controlla con
    /// [`ProtocolRange::is_valid`] o in decodifica.
    pub const fn new(min: Version, max: Version) -> Self {
        Self { min, max }
    }

    /// Versione minima supportata.
    pub const fn min(self) -> Version {
        self.min
    }

    /// Versione massima supportata.
    pub const fn max(self) -> Version {
        self.max
    }

    /// Vero se l'intervallo è valido: una sola maggiore, minimo non superiore
    /// al massimo e componenti nei limiti.
    pub const fn is_valid(self) -> bool {
        self.min.is_valid()
            && self.max.is_valid()
            && self.min.major == self.max.major
            && self.min.minor <= self.max.minor
    }

    /// Vero se la versione cade nell'intervallo.
    pub const fn contains(self, version: Version) -> bool {
        version.major == self.min.major
            && version.minor >= self.min.minor
            && version.minor <= self.max.minor
    }

    /// Sceglie la versione più alta in comune. Una maggiore diversa, o un
    /// intervallo senza versioni comuni, produce un errore.
    pub fn negotiate(self, other: Self) -> Result<Version, NegotiationError> {
        if !self.is_valid() || !other.is_valid() {
            return Err(NegotiationError {
                ours: self,
                theirs: other,
            });
        }
        if self.min.major != other.min.major {
            return Err(NegotiationError {
                ours: self,
                theirs: other,
            });
        }
        let low = self.min.minor.max(other.min.minor);
        let high = self.max.minor.min(other.max.minor);
        if low > high {
            return Err(NegotiationError {
                ours: self,
                theirs: other,
            });
        }
        Ok(Version::new(self.min.major, high))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProtocolRange {
    min: Version,
    max: Version,
}

impl<'de> Deserialize<'de> for ProtocolRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawProtocolRange::deserialize(deserializer)?;
        let range = ProtocolRange::new(raw.min, raw.max);
        if !range.is_valid() {
            return Err(serde::de::Error::custom(
                "protocol ranges must cover one major version with min <= max",
            ));
        }
        Ok(range)
    }
}

impl fmt::Display for ProtocolRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}..={}", self.min, self.max)
    }
}

/// Nessuna versione in comune fra i due intervalli.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegotiationError {
    pub ours: ProtocolRange,
    pub theirs: ProtocolRange,
}

impl fmt::Display for NegotiationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "no common protocol version between {} and {}",
            self.ours, self.theirs
        )
    }
}

impl std::error::Error for NegotiationError {}

impl From<NegotiationError> for ProtocolError {
    fn from(error: NegotiationError) -> Self {
        ProtocolError::protocol_incompatible(error.to_string())
    }
}

/// Versione della superficie A supportata da questo crate.
pub const ADMIN_SURFACE_VERSION: Version = Version::new(1, 0);

/// Versione della superficie C supportata da questo crate.
pub const ADAPTER_SURFACE_VERSION: Version = Version::new(1, 0);

/// Intervallo supportato dalla superficie A.
pub const ADMIN_SUPPORTED_RANGE: ProtocolRange =
    ProtocolRange::new(ADMIN_SURFACE_VERSION, ADMIN_SURFACE_VERSION);

/// Intervallo supportato dalla superficie C.
pub const ADAPTER_SUPPORTED_RANGE: ProtocolRange =
    ProtocolRange::new(ADAPTER_SURFACE_VERSION, ADAPTER_SURFACE_VERSION);

/// Sceglie la versione più alta in comune fra due intervalli.
pub fn negotiate(ours: ProtocolRange, theirs: ProtocolRange) -> Result<Version, NegotiationError> {
    ours.negotiate(theirs)
}
