//! Snapshot sintetici del journal (D4).
//!
//! Uno snapshot dichiara l'epoch, lo schema del proprio blob e l'ultima
//! sequenza effettivamente applicata, insieme allo stato prodotto da un reducer
//! deterministico. È pubblicato in un'unica transazione redb a partire da una
//! vista coerente degli eventi: la stessa transazione legge floor e high-water,
//! riprende lo snapshot precedente della stessa epoch e applica soltanto il
//! suffisso mancante.
//!
//! Regole:
//!
//! - lo stato è cumulativo dall'inizio dell'epoch: uno snapshot successivo
//!   riprende quello precedente, quindi caricare lo snapshot e applicare il
//!   suffisso produce lo stesso stato del replay completo;
//! - nessuno snapshot riusa una vista di un'epoch diversa, e nessuno snapshot
//!   viene accettato con una posizione dichiarata diversa dalla propria chiave;
//! - sono conservati al massimo [`MAX_SNAPSHOTS`] snapshot, i più recenti: gli
//!   altri vengono rimossi nella stessa transazione;
//! - il reducer è sintetico e non è il registro delle sessioni di prodotto.

use std::ops::Bound;

use redb::{ReadableDatabase, ReadableTable, Table};

use super::{
    EVENTS, Journal, JournalError, META, META_EPOCH, META_FLOOR, META_LAST, SNAPSHOTS, Step,
    checkpoint, corrupt, require_meta, storage,
};

/// Numero massimo di snapshot conservati.
pub(super) const MAX_SNAPSHOTS: usize = 2;

/// Versione dello schema del blob di snapshot.
const SNAPSHOT_SCHEMA: u64 = 1;
/// Cinque campi `u64`: schema, epoch, ultima sequenza, conteggio e checksum.
const SNAPSHOT_BYTES: usize = 5 * 8;

/// Stato sintetico prodotto dal reducer deterministico.
///
/// Il conteggio e il checksum dipendono dall'ordine e dal contenuto dei payload:
/// due sequenze diverse producono stati diversi, e lo stesso prefisso produce
/// sempre lo stesso stato.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReducerState {
    pub count: u64,
    pub checksum: u64,
}

impl ReducerState {
    /// Applica un payload allo stato. Deterministico e senza dipendenze
    /// esterne.
    pub fn apply(&mut self, payload: &[u8]) {
        self.count = self.count.wrapping_add(1);
        let mut checksum = self.checksum;
        for byte in payload {
            checksum ^= u64::from(*byte);
            checksum = checksum.wrapping_mul(0x0000_0100_0000_01B3);
        }
        self.checksum = checksum;
    }
}

/// Snapshot pubblicato: epoch, ultima sequenza applicata e stato.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub epoch: u64,
    pub applied_through: u64,
    pub state: ReducerState,
}

impl Journal {
    /// Produce e pubblica uno snapshot della vista corrente.
    ///
    /// Riprende lo snapshot più recente della stessa epoch e applica soltanto
    /// gli eventi successivi fino all'ultima sequenza della stessa transazione.
    /// Senza uno snapshot precedente e con un prefisso già rimosso non esiste
    /// uno stato cumulativo ricostruibile: l'esito è un rifiuto, non uno
    /// snapshot parziale. Lo snapshot può usare la riserva di manutenzione
    /// fino a [`JournalBudget::max_file_bytes`]; se la supera, la transazione
    /// già aperta viene annullata esplicitamente e l'esito è
    /// [`JournalError::CapacityExhausted`] soltanto se l'annullamento riesce,
    /// altrimenti [`JournalError::RollbackFailed`] con riapertura obbligatoria.
    pub fn snapshot(&mut self) -> Result<Snapshot, JournalError> {
        if self.recovery_required {
            return Err(JournalError::RecoveryRequired);
        }
        let started = self.file_bytes()?;
        let transaction = self.db.begin_write().map_err(storage)?;
        let snapshot = {
            let meta = transaction.open_table(META).map_err(storage)?;
            let epoch = require_meta(&meta, META_EPOCH)?;
            let floor = require_meta(&meta, META_FLOOR)?;
            let high_water = require_meta(&meta, META_LAST)?;
            let mut snapshots = transaction.open_table(SNAPSHOTS).map_err(storage)?;
            let (mut state, from) = match read_newest_snapshot(&snapshots, epoch)? {
                Some(previous) => {
                    if previous.applied_through < floor || previous.applied_through > high_water {
                        return Err(corrupt("snapshot fuori dal range conservato"));
                    }
                    (previous.state, previous.applied_through)
                }
                None => {
                    if floor != 0 {
                        return Err(corrupt(
                            "snapshot di partenza assente con un prefisso già rimosso",
                        ));
                    }
                    (ReducerState::default(), 0)
                }
            };
            let events = transaction.open_table(EVENTS).map_err(storage)?;
            for entry in events
                .range((Bound::Excluded(from), Bound::Included(high_water)))
                .map_err(storage)?
            {
                let (_, value) = entry.map_err(storage)?;
                state.apply(value.value());
            }
            let snapshot = Snapshot {
                epoch,
                applied_through: high_water,
                state,
            };
            snapshots
                .insert(high_water, encode(&snapshot).as_slice())
                .map_err(storage)?;
            trim_snapshots(&mut snapshots)?;
            snapshot
        };
        let transaction = self.guard_capacity(transaction, self.budget.max_file_bytes, started)?;
        checkpoint(Step::SnapshotBeforeCommit, &self.path)?;
        if let Err(error) = transaction.commit() {
            self.recovery_required = true;
            return Err(JournalError::CommitOutcomeUnknown(error));
        }
        checkpoint(Step::SnapshotAfterCommit, &self.path)?;
        Ok(snapshot)
    }

    /// Snapshot più recente, se esiste.
    pub fn latest_snapshot(&self) -> Result<Option<Snapshot>, JournalError> {
        if self.recovery_required {
            return Err(JournalError::RecoveryRequired);
        }
        let transaction = self.db.begin_read().map_err(storage)?;
        let meta = transaction.open_table(META).map_err(storage)?;
        let epoch = require_meta(&meta, META_EPOCH)?;
        let snapshots = transaction.open_table(SNAPSHOTS).map_err(storage)?;
        read_newest_snapshot(&snapshots, epoch)
    }
}

/// Legge lo snapshot con la sequenza più alta e ne verifica epoch e chiave.
pub(super) fn read_newest_snapshot(
    snapshots: &impl ReadableTable<u64, &'static [u8]>,
    epoch: u64,
) -> Result<Option<Snapshot>, JournalError> {
    let Some(entry) = snapshots.range(..).map_err(storage)?.next_back() else {
        return Ok(None);
    };
    let (key, value) = entry.map_err(storage)?;
    let record = decode(value.value()).ok_or_else(|| JournalError::Corrupt {
        detail: "snapshot non decodificabile".to_owned(),
    })?;
    if record.epoch != epoch {
        return Err(corrupt("snapshot di un'epoch diversa"));
    }
    if record.applied_through != key.value() {
        return Err(corrupt("snapshot con posizione incoerente"));
    }
    Ok(Some(record))
}

fn trim_snapshots(table: &mut Table<'_, u64, &'static [u8]>) -> Result<(), JournalError> {
    let keys = table
        .range(..)
        .map_err(storage)?
        .map(|entry| entry.map(|(key, _)| key.value()).map_err(storage))
        .collect::<Result<Vec<u64>, JournalError>>()?;
    let excess = keys.len().saturating_sub(MAX_SNAPSHOTS);
    for key in keys.into_iter().take(excess) {
        table.remove(key).map_err(storage)?;
    }
    Ok(())
}

fn encode(snapshot: &Snapshot) -> [u8; SNAPSHOT_BYTES] {
    let fields = [
        SNAPSHOT_SCHEMA,
        snapshot.epoch,
        snapshot.applied_through,
        snapshot.state.count,
        snapshot.state.checksum,
    ];
    let mut bytes = [0u8; SNAPSHOT_BYTES];
    for (chunk, field) in bytes.chunks_exact_mut(8).zip(fields) {
        chunk.copy_from_slice(&field.to_le_bytes());
    }
    bytes
}

/// Decodifica un blob di snapshot; `None` per schema o lunghezza diversi.
pub(super) fn decode(bytes: &[u8]) -> Option<Snapshot> {
    if bytes.len() != SNAPSHOT_BYTES {
        return None;
    }
    let mut fields = [0u64; 5];
    for (field, chunk) in fields.iter_mut().zip(bytes.chunks_exact(8)) {
        *field = u64::from_le_bytes(chunk.try_into().expect("chunk di 8 byte"));
    }
    if fields[0] != SNAPSHOT_SCHEMA {
        return None;
    }
    Some(Snapshot {
        epoch: fields[1],
        applied_through: fields[2],
        state: ReducerState {
            count: fields[3],
            checksum: fields[4],
        },
    })
}
