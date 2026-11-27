//! Identificatori opachi e valori con formato verificabile.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// Valore con formato non valido, con il nome del campo che lo contiene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidValue {
    pub field: &'static str,
    pub reason: &'static str,
}

impl fmt::Display for InvalidValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid {}: {}", self.field, self.reason)
    }
}

impl std::error::Error for InvalidValue {}

macro_rules! identifier_newtype {
    ($name:ident, $documentation:literal) => {
        #[doc = $documentation]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Crea l'identificatore se rispetta il formato di lavoro.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidValue> {
                let value = value.into();
                if value.is_empty() {
                    return Err(InvalidValue {
                        field: stringify!($name),
                        reason: "must not be empty",
                    });
                }
                if value.len() > crate::limits::MAX_ID_BYTES {
                    return Err(InvalidValue {
                        field: stringify!($name),
                        reason: "is longer than the working limit",
                    });
                }
                let valid = value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
                });
                if !valid {
                    return Err(InvalidValue {
                        field: stringify!($name),
                        reason: "contains characters outside [A-Za-z0-9._:-]",
                    });
                }
                Ok(Self(value))
            }

            /// Restituisce l'identificatore come stringa.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = InvalidValue;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

identifier_newtype!(MessageId, "Identificatore del singolo messaggio.");
identifier_newtype!(
    CorrelationId,
    "Identificatore del messaggio a cui si risponde."
);
identifier_newtype!(DiscoveryId, "Identificatore di una richiesta di discovery.");
identifier_newtype!(
    OperationId,
    "Identificatore di un'operazione hub verso adapter."
);
identifier_newtype!(SessionId, "Identificatore di una sessione Totum.");
identifier_newtype!(ProjectId, "Identificatore di un progetto nel perimetro.");
identifier_newtype!(RuntimeInstanceId, "Identificatore di un'istanza runtime.");
identifier_newtype!(AdapterInstanceId, "Identificatore di un processo adapter.");
identifier_newtype!(ProductId, "Identificatore del prodotto dell'agente.");
identifier_newtype!(
    TechnicalSurfaceId,
    "Identificatore della superficie tecnica servita dall'adapter."
);
identifier_newtype!(RequestId, "Identificatore di una richiesta del runtime.");
identifier_newtype!(
    ConversationId,
    "Identificatore della conversazione persistita nel fornitore."
);
identifier_newtype!(
    ExecutionId,
    "Identificatore dell'esecuzione viva collegata alla conversazione."
);
identifier_newtype!(CapabilityId, "Identificatore di una capability dichiarata.");
