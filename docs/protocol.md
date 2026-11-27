# Contratti di lavoro delle superfici A e C

**Ambito: incremento D2, 26 novembre 2026.** Questa è una specifica di lavoro per il crate `totum-contract`, non un protocollo stabile. Descrive solo ciò che il crate codifica, decodifica e valida oggi: non esistono ancora trasporti, hub operativo, pairing, crittografia o adapter. La codifica scelta qui non fissa quella del futuro protocollo mobile o del relay.

## Ambito

- **Superficie A — amministrazione locale.** Solo richiesta e risposta di stato e identità dell'hub.
- **Superficie C — comunicazione adapter–hub.** Handshake, capability dichiarate, discovery, collegamento alla sessione, eventi, operazioni e loro esiti, liveness.
- **Fuori ambito.** Trasporti, socket, listener, registry, journal, autorizzazione operativa, pairing, crittografia, identità dei controller, persistenza e qualunque implementazione dell'app mobile o del relay.

## Envelope

Ogni messaggio è un singolo oggetto JSON con i campi dell'envelope e i campi del payload allo stesso livello.

| Campo | Tipo | Presenza |
| --- | --- | --- |
| `surface` | `"admin"` oppure `"adapter"` | sempre |
| `version` | `{ "major": n, "minor": n }` | sempre |
| `id` | identificatore del messaggio | sempre |
| `type` | nome del tipo sulla superficie | sempre |
| `correlationId` | identificatore del messaggio a cui si risponde | solo nelle risposte della superficie A |

Regole verificate dalla decodifica:

- i campi sconosciuti sono ignorati, così un'aggiunta minore non interrompe un lettore precedente;
- una superficie o un tipo di messaggio sconosciuti sono rifiutati: un tipo noto soltanto su un'altra superficie è sconosciuto;
- un campo obbligatorio mancante o con tipo errato è rifiutato;
- la versione dell'envelope deve appartenere alla maggiore supportata dalla superficie;
- il tipo dichiarato nell'envelope deve coincidere con quello del corpo: la codifica rifiuta un messaggio incoerente invece di produrre un documento che la decodifica interpreterebbe come un altro tipo;
- un messaggio più grande del limite di lavoro di 64 KiB è rifiutato prima del parsing.

Esempio di richiesta di stato ([fixture](../crates/totum-contract/tests/fixtures/valid/admin-get-status.json)):

```json
{
  "surface": "admin",
  "version": { "major": 1, "minor": 0 },
  "id": "fixture-admin-request",
  "type": "getStatus"
}
```

### Correlazione

- `statusReport` e `hubIdentityReport` richiedono `correlationId` uguale all'`id` della richiesta.
- Le richieste della superficie A non devono portare `correlationId`.
- Sulla superficie C la risposta si correla con `discoveryId` o `operationId` nel payload; l'envelope non viene usato per la correlazione.

La decodifica controlla la presenza e l'assenza di `correlationId`; l'uguaglianza con la richiesta è verificata da chi gestisce la conversazione e dai test sulle fixture.

## Versioni e negoziazione

- Una versione è `{ major, minor }` con componenti da 1 a 999. La compatibilità richiede la **stessa maggiore**: una maggiore sconosciuta non è mai compatibile.
- Un intervallo `supportedProtocol` / `hubSupported` copre una sola maggiore, con minimo non superiore al massimo.
- La negoziazione sceglie la **versione più alta in comune** fra i due intervalli. Se non esiste, o se le maggiori differiscono, il risultato è l'errore applicativo `protocolIncompatible`.
- Questa revisione supporta `1.0` per la superficie A e `1.0` per la superficie C, con intervallo `1.0..=1.0`. Una differenza minore è additiva soltanto entro la stessa maggiore; un lettore precedente ignora i campi nuovi ma rifiuta i tipi di messaggio nuovi.
- La negoziazione non finge compatibilità: una versione sconosciuta produce un rifiuto, non una selezione arbitraria.

## Superficie A: amministrazione locale

| Tipo | Direzione | Payload |
| --- | --- | --- |
| `getStatus` | richiesta | nessun campo |
| `statusReport` | risposta | `hubVersion`, `identity` |
| `getHubIdentity` | richiesta | nessun campo |
| `hubIdentityReport` | risposta | `identity` |

`identity` è `{ "state": "notInitialized" }` oppure `{ "state": "present", "fingerprint": "..." }`. Per la prima esecuzione non esiste un valore finto: l'assenza di identità è uno stato esplicito. L'impronta è una stringa esadecimale di lunghezza pari, fra 16 e 128 caratteri: è un formato di lavoro, non una decisione sul futuro schema di identità o sulla crittografia.

Esempio di risposta ([fixture](../crates/totum-contract/tests/fixtures/valid/admin-status-report.json)):

```json
{
  "surface": "admin",
  "version": { "major": 1, "minor": 0 },
  "id": "fixture-admin-report",
  "correlationId": "fixture-admin-request",
  "type": "statusReport",
  "hubVersion": "0.1.0",
  "identity": {
    "state": "present",
    "fingerprint": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  }
}
```

## Superficie C: adapter–hub

### Handshake e capability

`adapterHello` dichiara l'istanza dell'adapter, il prodotto, la superficie tecnica servita, la versione dell'adapter, l'intervallo di protocollo e le capability dichiarate. `hubHello` risponde con la versione selezionata, l'intervallo dell'hub e le capability riconosciute per la connessione.

Distinzioni rappresentate:

- **prodotto** (`productId`) e **superficie tecnica** (`technicalSurfaceId`) sono identificatori diversi;
- una **capability dichiarata** dall'adapter non è una capability autorizzata: l'autorizzazione dell'hub non viaggia su questa superficie;
- una capability non riconosciuta resta un dato valido fra quelle dichiarate, ma non può comparire fra quelle riconosciute: la validazione di `hubHello` ammette solo capability note a questa revisione e la verifica dell'handshake richiede inoltre che l'adapter le abbia dichiarate; le capability sono identificatori aperti, così un elenco futuro non interrompe la decodifica;
- il riconoscimento non è un'autorizzazione: una capability riconosciuta dice soltanto che il contratto la comprende, non che l'operazione sia permessa.

Capability riconosciute da questa revisione: `discoverSessions`, `readStatus`, `readTimeline`, `readFiles`, `readDiff`, `receiveApproval`, `decideApproval`, `receiveInput`, `reply`, `startTask`, `pause`, `resume`, `stop`, `retry`.

### Discovery e sessioni

| Tipo | Direzione | Payload |
| --- | --- | --- |
| `discover` | hub → adapter | `discoveryId` |
| `discoveryResult` | adapter → hub | `discoveryId`, `runtimes`, `projects`, `sessions` |
| `attach` | hub → adapter | `sessionId` |
| `detach` | hub → adapter | `sessionId` |

Una istanza runtime dichiara `runtimeId`, prodotto, superficie tecnica, versione del runtime e stato (`available`, `degraded`, `unavailable`). Una sessione dichiara:

- `sessionId`, `projectId`, `runtimeId`;
- `mode`: `observed`, `prepared`, `managed`, `ended`;
- `state`: uno dei sette stati visivi `approval`, `error`, `input`, `working`, `completed`, `paused`, `offline`;
- `conversation.conversationId`, la conversazione persistita nel fornitore;
- `execution.executionId`, l'esecuzione viva collegata alla conversazione;
- `declaredCapabilities`, le capability dichiarate per quella sessione.

Due esecuzioni della stessa conversazione sono due sessioni: conversazione persistita ed esecuzione viva non vengono fuse. I riferimenti a progetto e runtime devono esistere nell'elenco dichiarato; identificatori duplicati o riferimenti sconosciuti sono rifiutati.

### Eventi, operazioni ed esiti

| Tipo | Direzione | Payload |
| --- | --- | --- |
| `sessionEvent` | adapter → hub | `sessionId`, `revision`, `event` |
| `operation` | hub → adapter | `operationId`, `sessionId`, `expiresAtUnixMs?`, `operation` |
| `operationAccepted` | adapter → hub | `operationId` |
| `operationRejected` | adapter → hub | `operationId`, `error` |
| `operationResult` | adapter → hub | `operationId`, `outcome` |
| `health` | bidirezionale | `state`, `detail?` |
| `ping` / `pong` | bidirezionale | `nonce` |
| `shutdown` | bidirezionale | `reason` |
| `error` | bidirezionale | `error` |

`sessionEvent.event` è `{ "kind": "updated", "state": ..., "mode": ... }` oppure `{ "kind": "ended", "completion": ... }`. La `revision` della sessione è distinta dall'identificatore del messaggio.

`operation.operation` è un'azione strutturata, **non un comando shell generico**:

- `approve` / `reject` con `requestId` e `requestRevision`; il rifiuto ammette un `reason` breve;
- `reply` con `requestId` e `text`;
- `stop`.

Il messaggio non trasporta identità, grant o autorizzazioni di un controller.

L'**accettazione** non è l'**esito**: `operationAccepted` dice che l'adapter ha preso in carico l'operazione; `operationResult` riporta ciò che è accaduto. L'esito è:

- `succeeded`, con riassunto facoltativo;
- `failed`, con un errore applicativo certo;
- `outcomeUnknown`, con un motivo (`noAcknowledgement`, `connectionLost`, `runtimeUnobservable`).

`outcomeUnknown` è distinto da un errore: non afferma che l'operazione sia fallita, dichiara che l'adapter non può stabilirne l'esito. Un rifiuto immediato usa `operationRejected` con un errore, prima di raggiungere il runtime.

Esempio di operazione ([fixture](../crates/totum-contract/tests/fixtures/valid/operation-approve.json)):

```json
{
  "surface": "adapter",
  "version": { "major": 1, "minor": 0 },
  "id": "fixture-operation",
  "type": "operation",
  "operationId": "operation-1",
  "sessionId": "session-1",
  "expiresAtUnixMs": 1893456000000,
  "operation": {
    "kind": "approve",
    "requestId": "request-1",
    "requestRevision": 2
  }
}
```

Esempio di esito incerto ([fixture](../crates/totum-contract/tests/fixtures/valid/operation-result-outcome-unknown.json)):

```json
{
  "surface": "adapter",
  "version": { "major": 1, "minor": 0 },
  "id": "fixture-operation-result-unknown",
  "type": "operationResult",
  "operationId": "operation-3",
  "outcome": { "outcome": "outcomeUnknown", "reason": "noAcknowledgement" }
}
```

## Errori

Due livelli distinti:

- **`DecodeError`** — errore locale di decodifica: messaggio troppo grande, JSON non valido, envelope non oggetto, campo mancante o invalido, superficie o tipo sconosciuti, limite superato, versione incompatibile. Non è un errore applicativo e non viene inviato come messaggio.
- **`ProtocolError`** — errore applicativo con `code`, `retryable` e `detail` facoltativo (massimo 512 caratteri). Viaggia dentro `operationRejected` o `error`.

Codici applicativi di questa revisione: `protocolIncompatible`, `invalidPayload`, `unsupportedCapability`, `targetUnavailable`, `staleRevision`, `requestExpired`, `alreadyResolved`, `rateLimited`, `adapterFailure`. `retryable` dichiara se il mittente ritiene sensato un nuovo tentativo; non è una garanzia. `outcomeUnknown` non è un codice di errore: è un esito dell'operazione.

La decodifica non deve produrre panici: lunghezza, parsing e conversione dei campi sono confinati e un input esterno malformato produce un errore strutturato.

## Limiti di lavoro

Sono parametri di lavoro, non valori misurati del prodotto; potranno cambiare dopo i primi esperimenti.

| Limite | Valore |
| --- | --- |
| Messaggio codificato | 64 KiB |
| Identificatore o capability | 128 byte, caratteri `[A-Za-z0-9._:-]` |
| Etichetta di versione (prodotto, runtime) | 64 byte, caratteri `[A-Za-z0-9.+-_]` |
| Impronta dell'hub | da 16 a 128 caratteri esadecimali, lunghezza pari |
| Dettaglio di errore | 512 caratteri |
| Etichetta breve (progetto, riassunto) | 256 caratteri |
| Testo di risposta a una richiesta di input | 4096 caratteri |
| Capability dichiarate per handshake o sessione | 64 |
| Istanze runtime in una discovery | 64 |
| Progetti in una discovery | 256 |
| Sessioni in una discovery | 512 |
| Componente di versione | 999 |

Gli identificatori sono opachi: il contratto non impone un formato Totum o del fornitore oltre ai caratteri ammessi.

## Fixture e test

Le fixture sono in [`crates/totum-contract/tests/fixtures/`](../crates/totum-contract/tests/fixtures/README.md), con dati interamente sintetici:

- `valid/` — un esempio per ogni messaggio supportato;
- `invalid/` — superficie e tipo sconosciuti, campi mancanti, tipi errati, JSON malformato, carattere di controllo, impronta non valida, correlazione mancante o non ammessa, intervallo invalido, capability duplicata o riconosciuta pur essendo sconosciuta;
- `incompatible/` — handshake con maggiore 2.0, rifiutato come incompatibile.

I test verificano i valori decodificati, non soltanto il round-trip del serializzatore; coprono inoltre i limiti, la negoziazione, i rifiuti di codifica e il rifiuto di input malformati senza panico. Le fixture e i tipi non sono uno schema stabile e non esiste alcun client Dart o di altre piattaforme in questo repository.

## Non obiettivi

- Nessun trasporto, socket, listener o servizio residente.
- Nessuna implementazione delle superfici mobile o relay e nessuna loro codifica fissata.
- Nessun hub operativo, registry, journal, pairing, crittografia o autorizzazione operativa.
- Nessun adapter reale e nessun comando che simuli funzionalità future.
