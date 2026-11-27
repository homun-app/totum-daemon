//! Libreria testabile del daemon.
//!
//! I moduli [`platform`], [`config`] e [`identity`] sono stati introdotti dai
//! passi P1–P7 di D3 e il passo P8 li collega al binario `totum-daemon`
//! attraverso [`startup`]: la radice di stato viene risolta, verificata,
//! ancorata e bloccata prima di ogni lettura o scrittura; identità e registro
//! vengono completati soltanto se mancano; la configurazione viene aperta e
//! recuperata o migrata sotto lo stesso lock; il processo resta residente senza
//! listener dopo la prontezza. `totum` mantiene il comportamento di D1. Dal
//! passo P5 `config::Store` richiede `platform::LockedRoot`, che possiede
//! insieme la radice ancorata e il lock esclusivo, e lo trattiene per tutta la
//! sua vita; dal passo P6 applica anche le migrazioni soltanto in avanti di uno
//! schema v0 sintetico. Dal passo P7 `identity` classifica `identity.json` e
//! `controllers.json`, espone gli stati tipizzati come
//! [`config::CoherenceInput`] e completa la sola prima inizializzazione
//! mancante sotto lo stesso lock. Il passo P9 aggiunge [`autostart`]: primitive
//! di libreria per stato, registrazione e rimozione dell'avvio automatico per
//! account, non collegate all'avvio del binario e senza comandi CLI.

pub mod autostart;
pub mod config;
pub mod identity;
pub mod platform;
pub mod startup;
