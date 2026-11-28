//! Retention del journal (D4).
//!
//! [`Journal::retain`] applica soglie di prova per byte logici e per età, con
//! l'orologio iniettato dal chiamante. Elimina soltanto il minimo prefisso che
//! soddisfa le due soglie, e soltanto se uno snapshot valido lo copre: la
//! rimozione degli eventi, dei loro tempi e degli snapshot superati avviene
//! nella stessa transazione che aggiorna il floor.
//!
//! Regole:
//!
//! - il floor è l'ultima sequenza rimossa; gli eventi conservati sono
//!   `(floor, high_water]` e la retention non rinumera né riutilizza sequenze;
//! - l'epoch non cambia: la retention non è un reset;
//! - senza uno snapshot che copra il prefisso necessario l'esito è
//!   [`RetentionOutcome::Blocked`] e nessun evento viene rimosso;
//! - l'età usa il tempo di ingestione assegnato dall'hub, mai un timestamp
//!   dell'agente; un orologio che torna indietro o un tempo futuro non anticipa
//!   cancellazioni;
//! - i byte logici sono la somma dei payload, non la dimensione fisica del file
//!   redb: una rimozione non promette una riduzione immediata del file;
//! - la dimensione fisica è letta dall'handle verificato e aperto dal backend,
//!   mai dal percorso nominale, ed è diagnostica: se non è disponibile dopo il
//!   commit, l'esito riporta comunque l'avanzamento già applicato.

use std::ops::Bound;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use redb::ReadableTable;

use super::{
    EVENTS, Journal, JournalError, META, META_EPOCH, META_FLOOR, META_LAST, SNAPSHOTS, Step, TIMES,
    checkpoint, require_meta, snapshot, storage,
};

/// Soglie di prova della retention: byte logici conservati e età massima.
///
/// Non sono valori predefiniti di prodotto: il chiamante li sceglie e
/// l'orologio è esplicito, così le prove sono deterministiche.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionLimits {
    /// Massimo dei byte logici degli eventi conservati.
    pub max_logical_bytes: u64,
    /// Età massima di un evento rispetto all'orologio del chiamante.
    pub max_age: Duration,
}

/// Esito della retention.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionOutcome {
    /// Nessun evento supera le soglie: il floor non cambia.
    Unchanged {
        floor: u64,
        logical_bytes: u64,
        /// Dimensione del file aperto, diagnostica e non promessa.
        physical_bytes: Option<u64>,
    },
    /// Il prefisso `(from_floor, to_floor]` è stato rimosso nella stessa
    /// transazione dell'avanzamento del floor. `physical_bytes` è misurato
    /// dopo il commit ed è diagnostico: `None` non annulla l'avanzamento.
    Advanced {
        from_floor: u64,
        to_floor: u64,
        removed: u64,
        logical_bytes_before: u64,
        logical_bytes_after: u64,
        physical_bytes: Option<u64>,
    },
    /// Servirebbe rimuovere un prefisso che nessuno snapshot valido copre:
    /// nessuna cancellazione. `snapshot_through` è l'ultima sequenza coperta
    /// dallo snapshot più recente, se ne esiste uno.
    Blocked {
        required_floor: u64,
        snapshot_through: Option<u64>,
        logical_bytes: u64,
        /// Dimensione del file aperto, diagnostica e non promessa.
        physical_bytes: Option<u64>,
    },
}

impl Journal {
    /// Applica la retention con le soglie e l'orologio indicati.
    ///
    /// Il commit è l'unico punto di verità dell'avanzamento. La dimensione
    /// fisica riportata dall'esito è una misura diagnostica dell'handle
    /// aperto: la sua indisponibilità non nasconde un floor già avanzato e non
    /// produce un errore.
    pub fn retain(
        &mut self,
        limits: RetentionLimits,
        now: SystemTime,
    ) -> Result<RetentionOutcome, JournalError> {
        if self.recovery_required {
            return Err(JournalError::RecoveryRequired);
        }
        let now_ms = millis(now);
        let max_age_ms = u64::try_from(limits.max_age.as_millis()).unwrap_or(u64::MAX);
        let transaction = self.db.begin_write().map_err(storage)?;

        let (epoch_floor_high, items, snapshot) = {
            let meta = transaction.open_table(META).map_err(storage)?;
            let epoch = require_meta(&meta, META_EPOCH)?;
            let floor = require_meta(&meta, META_FLOOR)?;
            let high_water = require_meta(&meta, META_LAST)?;
            drop(meta);
            let newest = {
                let snapshots = transaction.open_table(SNAPSHOTS).map_err(storage)?;
                snapshot::read_newest_snapshot(&snapshots, epoch)?
            };
            let items = {
                let events = transaction.open_table(EVENTS).map_err(storage)?;
                let times = transaction.open_table(TIMES).map_err(storage)?;
                let mut items = Vec::new();
                for entry in events
                    .range((Bound::Excluded(floor), Bound::Included(high_water)))
                    .map_err(storage)?
                {
                    let (key, value) = entry.map_err(storage)?;
                    let sequence = key.value();
                    let time = times
                        .get(sequence)
                        .map_err(storage)?
                        .ok_or_else(|| JournalError::Corrupt {
                            detail: format!(
                                "tempo di ingestione assente per la sequenza {sequence}"
                            ),
                        })?
                        .value();
                    items.push((sequence, value.value().len() as u64, time));
                }
                items
            };
            ((floor, high_water), items, newest)
        };
        let (floor, high_water) = epoch_floor_high;
        let logical_bytes_before = items
            .iter()
            .fold(0u64, |total, (_, len, _)| total.saturating_add(*len));

        let required = required_floor(
            floor,
            &items,
            logical_bytes_before,
            limits.max_logical_bytes,
            now_ms,
            max_age_ms,
        );
        let physical_bytes = self.physical_bytes();
        if required <= floor {
            return Ok(RetentionOutcome::Unchanged {
                floor,
                logical_bytes: logical_bytes_before,
                physical_bytes,
            });
        }

        match snapshot {
            Some(record)
                if record.applied_through >= required && record.applied_through <= high_water => {}
            Some(record) => {
                return Ok(RetentionOutcome::Blocked {
                    required_floor: required,
                    snapshot_through: Some(record.applied_through),
                    logical_bytes: logical_bytes_before,
                    physical_bytes,
                });
            }
            None => {
                return Ok(RetentionOutcome::Blocked {
                    required_floor: required,
                    snapshot_through: None,
                    logical_bytes: logical_bytes_before,
                    physical_bytes,
                });
            }
        }
        let dropped_bytes = items.iter().fold(0u64, |total, (sequence, len, _)| {
            if *sequence <= required {
                total.saturating_add(*len)
            } else {
                total
            }
        });

        // Rimozione e avanzamento del floor nella stessa transazione: gli
        // snapshot che coprono solo il prefisso rimosso non sono più
        // riutilizzabili e vengono eliminati; quello che copre `required`
        // resta.
        {
            let mut snapshots = transaction.open_table(SNAPSHOTS).map_err(storage)?;
            let stale = snapshots
                .range(..required)
                .map_err(storage)?
                .map(|entry| entry.map(|(key, _)| key.value()).map_err(storage))
                .collect::<Result<Vec<u64>, JournalError>>()?;
            for key in stale {
                snapshots.remove(key).map_err(storage)?;
            }
        }
        {
            let mut events = transaction.open_table(EVENTS).map_err(storage)?;
            events
                .retain_in(
                    (Bound::Excluded(floor), Bound::Included(required)),
                    |_, _| false,
                )
                .map_err(storage)?;
        }
        {
            let mut times = transaction.open_table(TIMES).map_err(storage)?;
            times
                .retain_in(
                    (Bound::Excluded(floor), Bound::Included(required)),
                    |_, _| false,
                )
                .map_err(storage)?;
        }
        {
            let mut meta = transaction.open_table(META).map_err(storage)?;
            meta.insert(META_FLOOR, required).map_err(storage)?;
        }

        checkpoint(Step::RetainBeforeCommit, &self.path)?;
        if let Err(error) = transaction.commit() {
            self.recovery_required = true;
            return Err(JournalError::CommitOutcomeUnknown(error));
        }
        checkpoint(Step::RetainAfterCommit, &self.path)?;
        Ok(RetentionOutcome::Advanced {
            from_floor: floor,
            to_floor: required,
            removed: required - floor,
            logical_bytes_before,
            logical_bytes_after: logical_bytes_before.saturating_sub(dropped_bytes),
            physical_bytes: self.physical_bytes(),
        })
    }
}

/// Calcola il minimo floor che soddisfa byte ed età, senza scendere sotto il
/// floor corrente. I tempi sono non decrescenti, quindi entrambe le soglie
/// identificano un prefisso.
fn required_floor(
    floor: u64,
    items: &[(u64, u64, u64)],
    logical_bytes: u64,
    max_logical_bytes: u64,
    now_ms: u64,
    max_age_ms: u64,
) -> u64 {
    let mut required = floor;
    if logical_bytes > max_logical_bytes {
        let target = logical_bytes - max_logical_bytes;
        let mut dropped = 0u64;
        for (sequence, len, _) in items {
            dropped = dropped.saturating_add(*len);
            required = *sequence;
            if dropped >= target {
                break;
            }
        }
    }
    for (sequence, _, time) in items {
        // Un tempo futuro (orologio tornato indietro) non è vecchio: la soglia
        // per età si ferma qui.
        let Some(age) = now_ms.checked_sub(*time) else {
            break;
        };
        if age < max_age_ms {
            break;
        }
        required = required.max(*sequence);
    }
    required
}

fn millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
