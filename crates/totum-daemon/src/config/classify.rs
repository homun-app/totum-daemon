//! Decisione di classificazione a partire dai due file osservati.
//!
//! L'ordine segue le tabelle del piano D3: percorso cambiato, schema futuro,
//! protezioni o tipo errati, revisione non valida o in diminuzione, schema del
//! principale precedente a quello del backup, stato dei componenti, coerenza;
//! poi principale valido, recupero, migrazione, prima esecuzione.

use super::{
    BackupState, CONFIG_BACKUP_FILE, CONFIG_FILE, CoherenceInput, Component, ComponentState,
    ConfigError, ConfigV1, Decision, GenerationFailure, MigrationSource, Observed, observe,
};
use crate::platform::VerifiedRoot;

pub(super) fn classify(root: &VerifiedRoot, coherence: CoherenceInput) -> Decision {
    let (main, backup) = match observe_pair(root) {
        Ok(pair) => pair,
        Err(decision) => return decision,
    };
    match precedence(main, backup) {
        Ok((main, backup)) => decide(main, backup, coherence),
        Err(decision) => decision,
    }
}

/// Classifica i soli due file, senza lo stato di identità e registro.
///
/// Usata dalla rivalidazione del salvataggio: il modulo `config` non rilegge
/// quei componenti, quindi non può rivalutarne la coerenza. Le regole sui due
/// file sono identiche a quelle della classificazione completa.
pub(super) fn classify_generations(root: &VerifiedRoot) -> Decision {
    let (main, backup) = match observe_pair(root) {
        Ok(pair) => pair,
        Err(decision) => return decision,
    };
    match precedence(main, backup) {
        Ok((main, backup)) => resolve(main, backup),
        Err(decision) => decision,
    }
}

fn observe_pair(root: &VerifiedRoot) -> Result<(Observed, Observed), Decision> {
    // Il percorso deve ancora indicare la radice ancorata: una rinomina o una
    // sostituzione non diventano mai assenza o prima esecuzione.
    if let Err(error) = root.verify_path() {
        return Err(Decision::Refused(ConfigError::Protection(error)));
    }
    Ok((
        observe(root, CONFIG_FILE),
        observe(root, CONFIG_BACKUP_FILE),
    ))
}

fn precedence(main: Observed, backup: Observed) -> Result<(Observed, Observed), Decision> {
    match (main, backup) {
        (
            Observed::TooNew {
                path,
                schema_version,
            },
            _,
        ) => Err(Decision::Refused(ConfigError::TooNew {
            path,
            schema_version,
        })),
        (
            _,
            Observed::TooNew {
                path,
                schema_version,
            },
        ) => Err(Decision::Refused(ConfigError::TooNew {
            path,
            schema_version,
        })),
        (Observed::Refused(error), _) | (_, Observed::Refused(error)) => {
            Err(Decision::Refused(error))
        }
        (Observed::Current(main), Observed::Current(backup))
            if backup.revision() > main.revision() =>
        {
            Err(Decision::Refused(ConfigError::RevisionRegression {
                main: main.revision(),
                backup: backup.revision(),
            }))
        }
        // Il principale è più vecchio del backup: migrarlo scarterebbe una
        // generazione di schema più recente. Nessuna migrazione, nessun
        // recupero.
        (Observed::Old(main), Observed::Current(backup)) => {
            Err(Decision::Refused(ConfigError::SchemaRegression {
                main: main.schema_version(),
                backup: backup.schema_version(),
            }))
        }
        (main, backup) => Ok((main, backup)),
    }
}

fn decide(main: Observed, backup: Observed, coherence: CoherenceInput) -> Decision {
    for (component, state) in components(coherence) {
        if matches!(state, ComponentState::NonInitial | ComponentState::Unusable) {
            return Decision::Refused(ConfigError::ComponentState { component, state });
        }
    }

    if present(&main) || present(&backup) {
        for (component, state) in components(coherence) {
            if state == ComponentState::Absent {
                return Decision::Refused(ConfigError::IncoherentState { component });
            }
        }
    }

    resolve(main, backup)
}

fn resolve(main: Observed, backup: Observed) -> Decision {
    match (main, backup) {
        (Observed::Absent, Observed::Absent) => Decision::FirstRun {
            config: ConfigV1::initial(),
        },
        (Observed::Current(config), backup) => Decision::Normal {
            config,
            backup: backup_state(backup),
        },
        // Il principale ha lo schema precedente riconosciuto: è lui la
        // sorgente. Un backup valido con schema più recente è già stato
        // rifiutato in precedenza.
        (Observed::Old(previous), _) => Decision::MigrationRequired {
            from_schema_version: previous.schema_version(),
            source: MigrationSource::Main,
            main_failure: None,
        },
        (Observed::Absent, Observed::Old(previous)) => Decision::MigrationRequired {
            from_schema_version: previous.schema_version(),
            source: MigrationSource::Backup,
            main_failure: Some(GenerationFailure::Absent),
        },
        (Observed::Corrupt { detail }, Observed::Old(previous)) => Decision::MigrationRequired {
            from_schema_version: previous.schema_version(),
            source: MigrationSource::Backup,
            main_failure: Some(GenerationFailure::Corrupt { detail }),
        },
        (Observed::Absent, Observed::Current(backup)) => Decision::RecoveryRequired {
            backup,
            main: GenerationFailure::Absent,
        },
        (Observed::Corrupt { detail }, Observed::Current(backup)) => Decision::RecoveryRequired {
            backup,
            main: GenerationFailure::Corrupt { detail },
        },
        (Observed::Absent, backup) => Decision::Refused(ConfigError::NoValidGeneration {
            main: GenerationFailure::Absent,
            backup: failure(backup),
        }),
        (Observed::Corrupt { detail }, backup) => {
            Decision::Refused(ConfigError::NoValidGeneration {
                main: GenerationFailure::Corrupt { detail },
                backup: failure(backup),
            })
        }
        (Observed::TooNew { .. } | Observed::Refused(_), _) => {
            unreachable!("i rifiuti sono già stati valutati")
        }
    }
}

fn components(coherence: CoherenceInput) -> [(Component, ComponentState); 2] {
    [
        (Component::Identity, coherence.identity),
        (Component::Registry, coherence.registry),
    ]
}

fn present(observed: &Observed) -> bool {
    !matches!(observed, Observed::Absent)
}

fn backup_state(observed: Observed) -> BackupState {
    match observed {
        Observed::Absent => BackupState::Absent,
        Observed::Current(previous) => BackupState::Available {
            revision: previous.revision(),
        },
        Observed::Corrupt { detail } => BackupState::Corrupt { detail },
        Observed::Old(previous) => BackupState::OlderSchema {
            schema_version: previous.schema_version(),
        },
        Observed::TooNew { .. } | Observed::Refused(_) => {
            unreachable!("i rifiuti sono già stati valutati")
        }
    }
}

fn failure(observed: Observed) -> GenerationFailure {
    match observed {
        Observed::Absent => GenerationFailure::Absent,
        Observed::Corrupt { detail } => GenerationFailure::Corrupt { detail },
        Observed::Current(_) | Observed::Old(_) => {
            unreachable!("la generazione utilizzabile è già stata gestita")
        }
        Observed::TooNew { .. } | Observed::Refused(_) => {
            unreachable!("i rifiuti sono già stati valutati")
        }
    }
}
