# CLI, binari e avvio del daemon

**Ambito: D1 e D3 P8, 27 novembre 2026.** Descrive ciò che i due binari fanno oggi. `totum-daemon` senza argomenti inizializza lo stato per account e resta residente senza listener, trasporti, adapter o protocollo wire. Hub operativo, installer, pairing, crittografia e servizio di sistema non esistono: nessun comando li simula. L'avvio automatico esiste soltanto come API di libreria (`totum_daemon::autostart`) e non è collegato al binario né a un comando.

## Invocazioni

| Invocazione | Effetto | Codice |
| --- | --- | --- |
| `totum-daemon --version` | stampa `totum-daemon` e la versione del pacchetto; nessun accesso allo stato | 0 |
| `totum-daemon` | risolve, verifica, ancora e blocca la radice, inizializza o recupera lo stato sotto il lock, segnala l'esito su standard error e resta residente | 0 all'avvio riuscito; 1 se l'avvio è rifiutato |
| `totum --version` | stampa `totum` e la versione della CLI | 0 |
| `totum status` | dichiara che il collegamento al daemon non è implementato e che nessun controllo di raggiungibilità viene eseguito | 0 |
| argomenti non supportati | messaggio su standard error con l'argomento rifiutato e l'uso; nessun accesso allo stato | 2 |

Qualunque altra combinazione di argomenti è rifiutata prima di toccare il file system: opzioni sconosciute, comandi sconosciuti, argomenti in più dopo `--version` o dopo `status`, comando mancante per `totum`.

## Ordine di avvio

`totum-daemon` senza argomenti esegue, nello stesso ordine del piano D3:

1. risoluzione della radice di stato dell'account dalle variabili di profilo del sistema;
2. verifica degli antenati, rifiuto dell'esecuzione elevata, creazione protetta dei soli componenti mancanti e ancoraggio della radice;
3. acquisizione del lock esclusivo su `state.lock`, senza attesa; una seconda istanza è rifiutata con `AlreadyRunning`;
4. classificazione completa di identità, registro, configurazione e backup;
5. completamento dei soli file iniziali mancanti, prima `identity.json` e poi `controllers.json`, senza riscrivere quelli presenti;
6. riclassificazione della coerenza di identità e registro;
7. apertura di `config::Store` con la coerenza aggiornata: prima esecuzione a revisione 0, caricamento normale, recupero dal backup o migrazione in avanti;
8. verifica finale dello stato e prontezza.

Nessun documento viene letto o scritto prima dei controlli di percorso, elevazione, ancoraggio e lock. Un rifiuto non è mai seguito dalle scritture dei passi successivi e non produce la prontezza. Lo `Store` e la radice bloccata restano vivi per tutta la durata del processo.

## Output

### Prima esecuzione

Standard error, nell'ordine:

```text
totum-daemon: prima esecuzione: creato identity.json
totum-daemon: prima esecuzione: creato controllers.json
totum-daemon: prima esecuzione: config.json creato alla revisione 0
totum-daemon 0.1.0 pronto: radice di stato /Users/esempio/Library/Application Support/Totum; nessun listener di rete
```

Il percorso mostrato è quello della piattaforma; l'esempio è macOS.

### Riavvio

Una riapertura senza scritture stampa soltanto la riga di prontezza:

```text
totum-daemon 0.1.0 pronto: radice di stato /Users/esempio/Library/Application Support/Totum; nessun listener di rete
```

### Backup inutilizzabile

Con `config.json` valido e `config.json.prev` corrotto l'avvio è consentito: il principale resta la generazione corrente e il backup non viene letto ulteriormente, riscritto, rimosso o recuperato. Prima della prontezza viene segnalato:

```text
totum-daemon: config.json.prev inutilizzabile: principale valido e backup non interpretabile (<dettaglio>); il backup non viene riparato
totum-daemon 0.1.0 pronto: radice di stato ...; nessun listener di rete
```

Il dettaglio proviene dalla classificazione già eseguita sul backup, senza una seconda lettura. Un backup assente, valido o con schema precedente riservato non produce l'avviso.

### Recupero e migrazione

```text
totum-daemon: recupero: config.json ripristinato dal backup alla revisione 3 (principale corrotto)
totum-daemon: copia del principale precedente conservata come config.corrupt.<epoch>.<contatore>
totum-daemon 0.1.0 pronto: radice di stato ...; nessun listener di rete
```

```text
totum-daemon: migrazione: config.json trasformato dallo schema 0 alla revisione 1 (revisione precedente 0)
totum-daemon 0.1.0 pronto: radice di stato ...; nessun listener di rete
```

### Durabilità non confermata

Dopo un commit la cui voce di directory non è stata sincronizzata, la riga di prontezza è preceduta da un avviso esplicito:

```text
totum-daemon: config.json pubblicato alla revisione 1 con commit effettuato ma durabilità non confermata: <dettaglio>; non è un errore e non viene ritentato
```

Il file è pubblicato: l'operazione non è fallita e non esiste un nuovo tentativo automatico. Lo stesso avviso esiste per un file iniziale di identità o registro.

### Errore di avvio

```text
totum-daemon: avvio non riuscito: schema 2 successivo a 1 in /Users/esempio/Library/Application Support/Totum/config.json: nessuna reinterpretazione
```

Il messaggio nomina il controllo fallito e, quando disponibile, il percorso e la causa di sistema. `totum-daemon` termina con codice 1 e non stampa la prontezza. La seconda istanza produce un messaggio che nomina `state.lock` e non modifica lo stato.

### Versione e uso non valido

`totum-daemon --version`:

```text
totum-daemon 0.1.0
```

`totum --version`:

```text
totum 0.1.0
```

`totum status`:

```text
totum 0.1.0
Collegamento al daemon: non implementato.
Nessun controllo di raggiungibilità è eseguito in questo scaffold.
```

`totum status --json` (argomento non supportato):

```text
totum: argomenti non supportati per "status": --json
Uso: totum <status | --version>
```

## Standard output e standard error

- `totum-daemon --version` scrive la versione su standard output e nulla su standard error.
- `totum-daemon` senza argomenti scrive su standard error le segnalazioni e la prontezza; **standard output resta vuoto**.
- Un uso non valido non scrive nulla su standard output: il messaggio e l'uso vanno su standard error.
- Il processo residente non legge lo standard input.

## Terminazione

`totum-daemon` non installa gestori di segnale e non stampa un messaggio di arresto.

- Su Unix la terminazione predefinita dei segnali (per esempio `SIGTERM` o `SIGINT`) chiude il processo; il sistema operativo rilascia il lock e `state.lock` non viene cancellato né riscritto.
- Su Windows la chiusura della console o del processo ha lo stesso effetto; non esiste un percorso di arresto ordinato diverso.
- Un risveglio non richiesto dell'attesa non termina il processo: l'attesa riprende.

## Codici di uscita

| Codice | Significato |
| --- | --- |
| 0 | Versione stampata, oppure avvio riuscito: prontezza raggiunta. Per il processo residente è il codice dell'eventuale uscita ordinaria, non della permanenza in esecuzione |
| 1 | Avvio rifiutato o fallito prima della prontezza: percorso, elevazione, lock, classificazione, inizializzazione, recupero o migrazione |
| 2 | Uso non valido: comando o argomento non supportato; nessun accesso allo stato |

Una terminazione per segnale non produce un codice di uscita del processo: è il segnale a essere riportato dal sistema.

## Versione

La versione stampata è quella dei pacchetti del workspace (`0.1.0` in questa fase), non una versione di protocollo o di prodotto. Il formato è `nome versione` seguito da un fine riga, come gli strumenti Unix abituali.

## Ciò che questi comandi non fanno

- `totum-daemon` non apre porte, socket o pipe, non avvia processi e non contatta la rete.
- `totum-daemon` non si registra per l'avvio automatico, non si installa e non crea un servizio di sistema. Le primitive di `totum_daemon::autostart` non sono richiamate dal binario e non esiste un comando che le esponga.
- `totum status` non tenta connessioni e non riporta salute, impronta o versione di un hub: riporta soltanto che il collegamento non è implementato.
- Nessun comando di pairing, diagnostica, policy, controller o adapter è disponibile; non vengono aggiunti comandi per funzionalità future.
- I percorsi reali dell'account non vanno usati nelle prove: il percorso operativo si verifica soltanto con radici temporanee.
