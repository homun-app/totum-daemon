# Totum Daemon

**All your agents. One place.**

Repository pubblico del daemon Rust di [Totum](https://homun.app/), destinato ai computer macOS, Windows e Linux per il monitoraggio e il controllo remoto degli agenti AI.

## Stato

Fondazione documentale, 25 novembre 2026. Il daemon non è ancora implementato: non sono disponibili crate compilabili, installer, binari o integrazioni operative. Sistemi e architetture supportati saranno verificati prima di una release.

Responsabilità previste: identità e pairing del computer, sessioni, stream di eventi, autorizzazione e deduplica dei comandi, collegamento agli agenti tramite interfacce definite. Il successo di un comando deve provenire dal runtime reale.

## Perimetro pubblico

Questo repository è dedicato al daemon. L’app mobile, il design system, il relay e il resto del prodotto rimangono privati. Non contiene il prototipo visivo, asset dell’app, documentazione interna, dati di sessioni reali o credenziali.

Prima dello sviluppo verranno definite le interfacce minime necessarie per compilare il daemon senza accesso a sorgenti privati. Non sono ancora selezionati schema wire, dipendenze Rust, distribuzione degli adapter o toolchain.

## Sviluppo

Per ora si possono modificare soltanto documentazione e configurazione del repository. Non eseguire `cargo build`: il manifest verrà aggiunto quando inizierà la fase applicativa.

Prima di un commit eseguire `git diff --check`. Le future build macOS/Windows/Linux e la pubblicazione degli installer saranno configurate dopo l’implementazione e la validazione della matrice di supporto.

## Licenza

La licenza del daemon non è ancora stata scelta. La visibilità pubblica non equivale all’assegnazione di una licenza open source; nessuna licenza di altri prodotti Homun viene applicata automaticamente.
