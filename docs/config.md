# Modulo `config` (D3, passi P3, P4, P5 e P6)

Stato al 27 novembre 2026: **libreria implementata e provata su macOS; usata dal percorso operativo del daemon dal passo P8**. Copre P3 (schema v1, validazione e classificazione in sola lettura), P4 (prima creazione, salvataggi, backup, sostituzione atomica, recupero persistito e gestione dei temporanei), P5 (costruzione obbligatoria sotto il lock esclusivo della radice, trattenuto per tutta la vita dello `Store`) e P6 (migrazioni soltanto in avanti con schema sintetico v0, trasformazione pura, conservazione dei byte originali nel backup e persistenza sotto lo stesso lock). Dal passo P7 il modulo `identity` classifica `identity.json` e `controllers.json` e fornisce gli stati tipizzati descritti in [identity.md](identity.md); la configurazione continua a non leggerli né a fissarne il formato. Dal passo P8 `totum-daemon` apre lo `Store` con la coerenza aggiornata dopo aver completato i file iniziali, applicando prima esecuzione, recupero o migrazione sotto lo stesso lock operativo; `totum` conserva il comportamento D1. Il lock è consultivo e coordina le istanze di Totum che rispettano il protocollo: non protegge da software dello stesso account che lo ignora.

La correzione del 27 novembre 2026 ha reso impossibile associare il lock a una radice diversa: `Store::open` accetta soltanto `platform::LockedRoot`, l'oggetto che possiede insieme la directory ancorata e il descrittore del lock. L'associazione è quella dell'identità della directory, non del percorso nominale; una seconda regressione verifica che un'altra directory con lo stesso percorso non sia protetta.

## Perimetro

Il modulo `totum_daemon::config`:

- definisce lo schema JSON v1 con i soli `schemaVersion` e `revision`;
- produce i valori iniziali della prima esecuzione e, dal passo P4, li persiste a revisione 0;
- legge `config.json` e `config.json.prev` con le primitive verificate del modulo [`platform`](platform.md), senza seguirne collegamenti e senza aggirarne i controlli di tipo, proprietario, modalità o ACL;
- ricontrolla che il percorso della radice coincida ancora con la directory ancorata prima di leggere e di scrivere;
- riconosce l'assenza soltanto tramite l'esito dedicato della piattaforma (`FileAbsent`), mai interpretando un cambio di percorso o un errore di sistema come assenza;
- limita la lettura a 64 KiB per file;
- valida campi, tipi, revisione e campi sconosciuti;
- riconosce uno schema futuro senza reinterpretarlo come configurazione corrente;
- riconosce uno schema precedente soltanto se il suo documento coincide esattamente con lo schema sintetico v0; qualsiasi altro contenuto è corrotto e non viene mai migrato;
- restituisce una decisione esplicita fra stato normale, prima esecuzione, recupero necessario, migrazione necessaria ed errore, indicando se la generazione da migrare è il principale o il backup;
- applica la decisione soltanto dopo averla ottenuta e sempre sotto il lock: prima creazione, recupero persistito, migrazione in avanti e pulizia dei temporanei del namespace riservato;
- migra soltanto in avanti, verso lo schema compreso, con una trasformazione pura e verificabile, incremento controllato della revisione e conservazione dei byte originali nel backup prima della sostituzione;
- rivalida i due file su disco prima di ogni salvataggio: schema futuro, protezioni errate, principale corrotto, migrazione necessaria e revisione diversa da quella caricata non autorizzano scritture, e né il recupero né la migrazione vengono mai applicati da un salvataggio;
- salva le generazioni successive con incremento controllato della revisione, backup della generazione corrente e sostituzione atomica del principale;
- distingue in modo esplicito un'operazione non commessa, un commit riuscito e un commit con durabilità non confermata;
- si costruisce soltanto con `platform::LockedRoot`, che possiede insieme la radice ancorata e il lock esclusivo ottenuto con `platform::lock`; lo trattiene per tutta la vita dello `Store` e non legge, classifica operativamente, pulisce o scrive prima dell'acquisizione.

Sono fuori perimetro: journal, avvio automatico, listener, pairing e crittografia, oltre alla concorrenza fra processi che non rispettano il lock. Il processo residente di P8 usa `Store` ma non espone comandi di salvataggio, recupero o migrazione. I formati di identità e registro sono definiti dal modulo `identity` (P7) e documentati in [identity.md](identity.md); non esistono comandi diagnostici che pilotino la scrittura: i punti di guasto sono interni alle prove della libreria.

## Formato v1

JSON UTF-8, un documento per file:

```json
{
  "schemaVersion": 1,
  "revision": 0
}
```

- `schemaVersion` deve essere un intero; v1 è l'unica versione compresa.
- Il documento deve essere un **oggetto JSON**: array, scalari e `null` sono corrotti e non vengono mai interpretati come configurazione o come sorgente di migrazione. La deserializzazione posizionale non è ammessa.
- `revision` deve essere un intero non negativo; identifica la generazione, non è un contatore di sicurezza né un'asserzione di unicità storica.
- Un campo sconosciuto, un campo duplicato, un campo mancante, un tipo errato o un documento troncato rendono il file inutilizzabile: non esistono valori predefiniti silenziosi.
- I limiti sono 64 KiB per file. Il limite esiste per evitare caricamenti illimitati e va rivisto quando arriveranno campi di dominio reali.

## Formato v0 sintetico

Nessuna versione distribuita di Totum ha mai scritto uno schema precedente al v1. Per provare il meccanismo di migrazione in avanti è definito uno schema **v0 esclusivamente sintetico**, che non rappresenta un formato storico reale e non è compatibile con alcun dato esistente:

```json
{
  "schemaVersion": 0
}
```

- Il documento deve essere un oggetto JSON e ammettere soltanto `schemaVersion: 0`: nessun altro campo, nessuna `revision`. Array, scalari, `null`, campi sconosciuti, duplicati, tipi errati o un documento troncato rendono la generazione corrotta e mai migrabile.
- La revisione di uno schema v0 è implicitamente 0. Senza `revision` la migrazione produce sempre la revisione 1 ed evita che una generazione migrata possa collidere con o superare un backup v1 valido.
- La migrazione v0 → v1 produce `{"schemaVersion":1,"revision":1}\n`.
- Uno schema `schemaVersion` inferiore a 0 o un documento v0 che non coincide esattamente con il formato sopra non sono riconosciuti come generazione precedente valida: sono corrotto o rifiuto, mai una reinterpretazione.

## Validazione e schema futuro

Prima di interpretare lo schema viene imposta la forma: il primo carattere dopo lo spazio JSON bianco deve essere `{`. Un array (per esempio `[0]`), uno scalare o `null` produzcono un documento corrotto, mai una generazione v1 né una sorgente migrabile. Il controllo non costruisce un valore intermedio, quindi i campi duplicati restano visibili alla deserializzazione e vengono rifiutati.

Il parsing avviene poi in due fasi. La prima legge soltanto l'intestazione: una `schemaVersion` maggiore di 1 produce `configTooNew` senza interpretare gli altri campi. La seconda valida il documento v1 completo con i campi sconosciuti e duplicati rifiutati.

Una `schemaVersion` inferiore a 1 è riconosciuta come generazione più vecchia soltanto se il documento coincide esattamente con lo schema v0 sintetico; in quel caso produce `MigrationRequired` con la provenienza (`Main` o `Backup`). Un contenuto diverso è corrotto: il modulo non ne interpreta i campi e non lo migra mai.

La revisione non valida è distinta dalla corruzione strutturale:

- `revision` negativa, non intera, non numerica o fuori intervallo produce un **rifiuto** (`InvalidRevision`), come richiede T8h;
- un documento malformato, con campi sconosciuti, duplicati o mancanti, è **corrotto** e può essere recuperato da un backup valido (P4), ma non viene mai interpretato in modo permissivo.

## Classificazione

La classificazione avviene senza scrivere. Ogni file viene classificato come assente, valido e supportato, corrotto, schema futuro, schema più vecchio, oppure rifiutato per tipo, proprietà, modalità o ACL. La decisione segue le tabelle del piano D3:

| Principale | Backup | Esito |
| --- | --- | --- |
| valido v1 | valido v1, revisione uguale o inferiore | `Normal`; il backup resta di riserva |
| valido v1 | assente | `Normal`; nessun backup disponibile |
| valido v1 | corrotto | `Normal`; backup inutilizzabile segnalato e non riparato |
| valido v1 | schema v0 sintetico | `Normal`; backup riservato (`OlderSchema`), mai migrato mentre il principale è valido |
| valido v1 | schema futuro | rifiuto `TooNew` |
| valido v1 | revisione superiore al principale | rifiuto `RevisionRegression` |
| corrotto o assente | valido v1 | `RecoveryRequired`; `Store::open` applica il recupero persistito |
| corrotto o assente | schema v0 sintetico | `MigrationRequired { source: Backup }`; `Store::open` conserva l'eventuale principale corrotto e migra il backup |
| schema v0 sintetico | qualunque, tranne v1 valido | `MigrationRequired { source: Main }`; `Store::open` migra il principale |
| schema v0 sintetico | valido v1 | rifiuto `SchemaRegression`; migrare il principale scarterebbe una generazione più nuova |
| corrotto o assente | assente o corrotto | rifiuto `NoValidGeneration` |
| assente | assente | `FirstRun`, con valori iniziali in memoria, solo se le condizioni di prima esecuzione sono soddisfatte |
| radice rinominata, rimossa o sostituita | qualunque | rifiuto `Protection(PathChanged)` |
| schema futuro, in principale o backup | qualunque | rifiuto `TooNew` |
| tipo, proprietà, modalità o ACL errati, in principale o backup | qualunque | rifiuto `Protection` |
| revisione non valida | qualunque | rifiuto `InvalidRevision` |

La precedenza dei rifiuti è: percorso cambiato, schema futuro, protezioni o tipo errati (compresi gli errori di lettura, l'accesso negato e il limite di dimensione), revisione non valida o in diminuzione, principale con schema precedente e backup con schema superiore (`SchemaRegression`), stato non iniziale o inutilizzabile di identità e registro, configurazione presente senza identità o registro. Un file più grande di 64 KiB produce `TooLarge` e non viene caricato oltre il limite.

### Assenza, percorso cambiato e rifiuti

L'assenza è un esito esplicito della piattaforma, non un'interpretazione dell'errore:

- `PlatformError::FileAbsent` significa che il file richiesto non esiste nella radice verificata: su Mac e Linux il descrittore è ancorato alla directory aperta, su Windows la radice viene ricontrollata prima dell'apertura;
- `PlatformError::PathChanged` significa che la radice o un componente del percorso non coincide più con la directory ancorata: rinomina, rimozione, sostituzione o percorso sparito fra ricontrollo e apertura. È sempre un rifiuto;
- accesso negato, collegamenti e reparse point, tipo non regolare, proprietario, modalità o ACL errati restano errori specifici del modulo `platform` e producono `Protection`.

Solo `FileAbsent` produce `Absent` nella classificazione. Un `PathChanged` o un errore di sistema non diventano mai prima esecuzione, recupero o caricamento del backup. La radice viene ricontrollata con `VerifiedRoot::verify_path` prima di aprire i due file, quindi la stessa regola vale su Mac, Linux e Windows.

Un temporaneo residuo (`.config.<pid>.<contatore>.tmp`) non viene mai letto né applicato: `classify` lo ignora e `Store::open` lo scarta dopo una decisione che ammette la scrittura (prima creazione, stato normale, recupero o migrazione), purché il nome appartenga al namespace riservato descritto sotto.

## Coerenza con identità e registro

Il modulo non legge né interpreta `identity.json` e `controllers.json` e non ne fissa il formato: dal passo P7 la classificazione dei due file e la produzione dello stato tipizzato appartengono al modulo `identity`, documentato in [identity.md](identity.md). `identity::Classification::coherence()` restituisce i due stati nel tipo `CoherenceInput`:

| Stato | Significato |
| --- | --- |
| `Absent` | file assente |
| `Initial` | file presente e valido con contenuto iniziale |
| `NonInitial` | contenuto non iniziale, non ammesso in D3 |
| `Unusable` | corrotto, schema futuro, tipo, proprietario o protezioni errate |

Con una configurazione presente, identità e registro devono essere `Initial`; uno stato `Absent` è uno stato incoerente e `NonInitial` o `Unusable` sono rifiutati. La prima esecuzione è ammessa soltanto quando non esiste né il principale né il backup e i due componenti sono assenti o iniziali. Un contenuto non iniziale presente senza configurazione è comunque un rifiuto: in D3 nessuno stato non iniziale è ammesso.

## Garanzie di sola lettura

- `config.json` e `config.json.prev` si aprono con `VerifiedRoot::open_file`: un collegamento simbolico è rifiutato, un tipo non regolare è un errore, proprietario e protezioni (modalità esattamente 0600 e ACL su Unix, DACL protetta su Windows) vengono verificati e mai riparati.
- Prima della lettura il percorso della radice viene ricontrollato con `VerifiedRoot::verify_path`: una rinomina, una rimozione o una sostituzione producono `PathChanged` e un rifiuto, non un'assenza.
- Un file mancante produce `PlatformError::FileAbsent` e diventa `Absent`; nessun altro errore viene interpretato come assenza.
- La lettura avviene sul descrittore ancorato alla radice verificata, con un limite di 64 KiB.
- La classificazione non crea, non rinomina, non elimina e non riscrive file. Le prove verificano che contenuto, presenza e modalità restino invariati in tutti i casi classificati, compresi recupero e rifiuto. Le scritture avvengono soltanto in `Store::open`, dopo una decisione che le ammette, e in `Store::save`, sempre con il guard esclusivo della radice già acquisito.
- Il percorso operativo del daemon apre lo `Store` dal passo P8, sotto lo stesso `LockedRoot`; non esistono tuttavia comandi che salvino, recuperino o migrino su richiesta, e lo `Store` non viene rilasciato finché il processo resta residente.

## Apertura, guard esclusivo e scrittore unico

`Store::open(locked, coherence)` riceve `platform::LockedRoot`, l'oggetto ottenuto da `platform::lock` che possiede insieme la radice già verificata e ancorata da `platform::prepare_root` e il suo lock esclusivo, più lo stato tipizzato di identità e registro. Il lock è obbligatorio per costruzione: la firma non ammette una radice senza lock né un lock separato dalla radice, perché `platform::lock` consuma la `VerifiedRoot` e restituisce un solo oggetto. `Store` lo conserva in un campo privato e non esiste un percorso che lo rilasci mentre lo `Store` resta utilizzabile. Se un'altra istanza detiene il lock, `platform::lock` produce `AlreadyRunning` prima di questa chiamata e la seconda istanza non arriva a classificare, pulire o scrivere: può aver preparato e verificato il contenitore, ma non ha letto né modificato configurazione, backup o temporanei. Applica una sola decisione e restituisce la generazione caricata:

| Decisione | Azione di `Store::open` |
| --- | --- |
| `Refused` | errore, nessuna scrittura; i temporanei non vengono toccati |
| `MigrationRequired` | scarta i temporanei del namespace riservato, poi applica la migrazione in avanti dalla provenienza indicata |
| `FirstRun` | scarta i temporanei del namespace riservato, poi crea la revisione 0 |
| `Normal` | scarta i temporanei del namespace riservato, carica il principale senza modificarlo |
| `RecoveryRequired` | scarta i temporanei del namespace riservato, poi applica il recupero persistito |

La classificazione in sola lettura di P3 resta richiamabile senza lock per ispezionare i file, ma non è un percorso di persistenza: prima creazione, recupero, pulizia e salvataggi esistono soltanto dietro il lock. Il rilascio avviene alla distruzione dello `Store` o alla morte del processo; `state.lock` resta al suo posto e la sua esistenza non indica un detentore. Il lock è legato alla open file description: resta detenuto finché l'ultimo descrittore che la referenzia è chiuso, quindi un processo forchettato che eredita il descrittore prima di `exec` lo trattiene per quella finestra. Il daemon non crea processi; le prove che ne creano coordinano l'avvio dei figli con le transizioni del lock e una prova di libreria fissa questa semantica.

Una collisione in prima creazione riporta alla classificazione, al massimo tre volte; nessun oggetto comparente viene sovrascritto. `Store::save` serializza inoltre le chiamate della stessa istanza con un `Mutex`. Ogni salvataggio riclassifica i due file e rifiuta una revisione del principale diversa da quella caricata: la rivalidazione resta una difesa in profondità e rileva modifiche avvenute per altre vie, come quelle di software che ignora il lock. La revisione identifica una generazione, non è un contatore di sicurezza.

## Prima creazione

Avviene soltanto dopo una classificazione `FirstRun`:

1. serializza la revisione 0;
2. crea `.config.<pid>.<contatore>.tmp` con creazione esclusiva, modalità 0600 imposta e verificata, scrive, sincronizza il file e chiude;
3. pubblica il temporaneo come `config.json` **solo se la destinazione non esiste**: `renameat2`/`renameatx_np` con `RENAME_NOREPLACE` su Linux e macOS, `MoveFileExW` senza `MOVEFILE_REPLACE_EXISTING` su Windows;
4. sincronizza la directory dove l'API lo consente.

Non esiste `config.json.prev` per la revisione 0. Se `config.json` compare fra la classificazione e la pubblicazione, l'esito è `AppearedDuringFirstRun`: il temporaneo viene rimosso, nulla viene sovrascritto e `Store::open` torna alla classificazione. Un errore in qualunque passo prima della pubblicazione significa che la prima creazione non è avvenuta: non resta alcun file di configurazione e il tentativo successivo riparte dalla classificazione.

## Salvataggio successivo

Da `revision: N` a `N+1`, con il principale presente:

1. verifica che il percorso della radice coincida ancora con quella ancorata;
2. rivalida i due file con la stessa precedenza della classificazione: schema futuro, protezioni o tipo errati, revisione non valida o in diminuzione, principale assente o corrotto e migrazione necessaria producono un errore **prima di creare temporanei**; se il principale è valido ma la sua revisione non è `N`, produce `RevisionDivergence`;
3. legge i byte del principale con le primitive verificate: sono il contenuto del backup;
4. crea e sincronizza il temporaneo della nuova generazione;
5. crea e sincronizza un secondo temporaneo con i byte della generazione corrente, quindi lo rinomina su `config.json.prev`; il principale non viene toccato;
6. sostituisce atomicamente il principale con il temporaneo nuovo;
7. sincronizza la directory.

La rivalidazione rifiuta lo stato che l'apertura avrebbe rifiutato e non adotta una revisione divergente: un principale con `revision` diversa da quella caricata produce `RevisionDivergence { expected, found }`; un principale assente o corrotto con backup valido produce `RecoveryRequired`, perché il recupero resta un percorso esplicito e non viene applicato da un salvataggio; un principale corrotto senza backup valido produce `NoValidGeneration`; una generazione v0 sintetica produce `MigrationRequired { from_schema_version, source }`. Identità e registro non vengono riletti: il modulo `config` non conosce i loro formati e la loro classificazione appartiene al modulo `identity` (P7). Il lock esclusivo impedisce a una seconda istanza cooperante di scrivere la stessa radice; la rivalidazione resta perché il lock è consultivo e non copre software che lo ignora.

Il backup è sempre la generazione immediatamente precedente a quella caricata. Fra il passo 5 e il passo 6 `config.json.prev` coincide con il principale e la nuova generazione non è ancora visibile; dopo la prima creazione `config.json` non è mai assente. Un errore prima della sostituzione del principale è un'operazione **non commessa**: il principale resta la generazione precedente, `config.json.prev` può essere già stato aggiornato e il chiamante non deve ritentare da solo. Se l'errore avviene proprio nella sostituzione del principale, `.prev` può coincidere con il principale: la coppia resta valida e il tentativo successivo la ricalcola.

## Commit, ritorno e durabilità

| Sistema | Punto di commit | Punto di ritorno e durabilità dichiarata |
| --- | --- | --- |
| Linux | sostituzione atomica di `config.json` | dopo `fsync` della directory; durabilità dipendente dal file system, non provata |
| macOS | sostituzione atomica di `config.json` | dopo `fsync` della directory; durabilità dichiarata solo con `F_FULLFSYNC` del file e `fsync` della directory riusciti |
| Windows | `MoveFileExW` con `MOVEFILE_REPLACE_EXISTING` | subito dopo la sostituzione; la voce di directory non è sincronizzata, quindi la durabilità non è dichiarata e l'esito è `CommittedNotDurable` |

`Commit::Committed` significa che il commit è avvenuto e la piattaforma ha confermato i passi di sincronizzazione. `Commit::CommittedNotDurable` significa che il commit è avvenuto ma la durabilità non è confermata: **non è un errore**, la revisione in memoria avanza e il salvataggio non va ritentato automaticamente. Un `Err` è sempre un'operazione non commessa. Dopo un commit, un eventuale guasto di sincronizzazione non viene mai presentato come salvataggio fallito.

La revisione al massimo (`u64::MAX`) produce `RevisionOverflow` prima di qualunque scrittura. Il recupero conserva la revisione della generazione caricata e non la rinumera; la migrazione incrementa di uno la revisione della generazione con schema precedente e rifiuta l'overflow con `MigrationRevisionOverflow`.

## Recupero persistito

Si applica soltanto a una decisione `RecoveryRequired`, cioè con il principale assente o corrotto e il backup valido:

1. verifica il percorso e rilegge i byte del backup con le primitive verificate;
2. se il principale esiste ed è corrotto, ne copia i byte in `config.corrupt.<epoch>.<contatore>` (file nuovo, 0600, sincronizzato, fino a 16 nomi); il principale **non viene spostato** e resta al suo posto;
3. scrive i byte del backup in un temporaneo, lo sincronizza, sostituisce il principale e sincronizza la directory;
4. lascia `config.json.prev` invariato: resta la generazione recuperata.

Se la conservazione della copia fallisce, il recupero non è commesso e il principale corrotto è intatto. Se fallisce dopo la copia, la copia resta come diagnostica e il principale resta corrotto; una nuova apertura ripete il recupero. Un recupero interrotto è idempotente. Un errore dopo la sostituzione del principale è `CommittedNotDurable`, mai un errore di recupero.

## Migrazioni in avanti

Si applicano soltanto a una decisione `MigrationRequired`, cioè quando una generazione v0 sintetica è la sorgente e non esiste un v1 valido che la renda obsoleta:

| Principale | Backup | Provenienza |
| --- | --- | --- |
| v0 sintetico | assente, corrotto o v0 sintetico | `Main` |
| assente o corrotto | v0 sintetico | `Backup` |
| valido v1 | v0 sintetico | nessuna migrazione: il backup resta riservato |
| v0 sintetico | valido v1 | rifiuto `SchemaRegression` |

La trasformazione è una funzione pura, `migrate_v0_to_v1`: la revisione 0 dello schema v0 diventa la revisione 1 dello schema v1 con incremento controllato; un overflow produce `MigrationRevisionOverflow` prima di qualunque scrittura. Il documento risultante è serializzato come un v1 ordinario.

La persistenza riusa le garanzie di P4 e avviene sotto lo stesso `LockedRoot` della classificazione:

1. ricontrolla il percorso della radice;
2. se la sorgente è il backup con un principale corrotto, conserva il principale in `config.corrupt.<epoch>.<contatore>` con la stessa regola del recupero; il principale non viene spostato;
3. crea e sincronizza il temporaneo della nuova generazione v1;
4. crea e sincronizza un secondo temporaneo con i **byte originali** della sorgente, quindi lo rinomina su `config.json.prev`;
5. sostituisce atomicamente il principale con il temporaneo v1;
6. sincronizza la directory.

La sorgente non viene mai sovrascritta prima che la nuova generazione sia stata scritta e sincronizzata. Migrando dal principale, il backup riceve i suoi byte originali v0; migrando dal backup, il backup riceve di nuovo i propri byte v0 e il principale corrotto resta conservato. In entrambi i casi la riapertura trova il principale v1 e il backup v0 riservato, quindi nessuna seconda migrazione viene applicata. Un errore in qualunque passo prima della sostituzione del principale è un'operazione non commessa: la generazione sorgente è intatta e i temporanei vengono rimossi. Un errore dopo la sostituzione del principale è `CommittedNotDurable`, mai un errore di migrazione: la revisione avanza e la migrazione non viene ritentata automaticamente.

La classificazione e la persistenza avvengono nella stessa `Store::open`, sotto un unico lock, quindi la sorgente non può cambiare fra decisione e scrittura. Una rivalidazione esplicita resta come difesa in profondità e rifiuta con `MigrationSourceChanged` una sorgente che non è più quella classificata; `Store::save` non applica mai una migrazione e restituisce `MigrationRequired { from_schema_version, source }` se la trova.

## Temporanei

`.config.<pid>.<contatore>.tmp` è il **namespace riservato** al modulo: prefisso `.config.`, due gruppi decimali separati da un punto e suffisso `.tmp`, nient'altro. Il PID fa parte del nome per riconoscere le esecuzioni, non è un criterio di proprietà: il codice riconosce il nome, non il processo che lo ha scritto.

La pulizia di `Store::open` elenca le voci della radice ancorata e rimuove **ogni** nome del namespace, indipendentemente dal PID. È coerente con la precondizione di un solo scrittore, ora garantita fra le istanze cooperanti dal lock esclusivo: lo `Store` si costruisce soltanto con `LockedRoot`, quindi non esistono temporanei attivi di altre istanze cooperative e un nome del namespace è sempre un residuo del modulo, di questa o di una precedente esecuzione. Un processo che ignora il lock non è protetto: i suoi temporanei sarebbero rimossi, come dichiarato nei limiti.

I file fuori dal namespace non vengono mai rimossi né applicati, comprese le copie `config.corrupt.*`, i file dell'utente e i nomi simili ma non conformi, come `x.config.1.0.tmp` o `.config.1.0.tmp.bak`. Un nome del namespace occupato da una directory produce un errore esplicito e non viene rimosso. Un temporaneo residuo non viene mai letto come generazione: non è `config.json` né `config.json.prev`.

## Riferimenti

- [`rename` POSIX](https://pubs.opengroup.org/onlinepubs/9699919799/functions/rename.html) e [`fsync` POSIX](https://pubs.opengroup.org/onlinepubs/9699919799/functions/fsync.html): sostituzione atomica e sincronizzazione.
- [`renameat2(2)` su Linux](https://man7.org/linux/man-pages/man2/renameat2.2.html): `RENAME_NOREPLACE`; [`fsync(2)`](https://man7.org/linux/man-pages/man2/fsync.2.html).
- [`renameatx_np(2)`](https://manpages.debian.org/unstable/manpages-dev/renameatx_np.2.en.html) e `fcntl(2)` con `F_FULLFSYNC` su macOS: pubblicazione senza sostituzione e durabilità del file.
- [`MoveFileExW`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-movefileexw) e [`FlushFileBuffers`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers): sostituzione e sincronizzazione su Windows.
- [`FindFirstFileW`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-findfirstfilew): elenco delle voci per la pulizia dei temporanei.
- [`File::try_lock`](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock) della libreria standard (stabile dalla 1.89): `flock` esclusivo non bloccante su Unix, `LockFileEx` con `LOCKFILE_FAIL_IMMEDIATELY` su Windows. Vedi anche [`flock(2)`](https://man7.org/linux/man-pages/man2/flock.2.html) e [`LockFileEx`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex).

Le API sono usate secondo la documentazione; la durabilità alla perdita di alimentazione non è stata provata su nessun sistema.

## Prove eseguite

Su macOS (Apple Silicon), toolchain 1.95.0, con `--locked --offline`:

- 38 test di libreria della configurazione (37 eseguiti e un helper di arresto ignorato, avviato soltanto dai processi figli delle prove): 14 sul parsing e sulla trasformazione (schema v1, nomi dei campi esatti, campi sconosciuti e duplicati, campi mancanti, revisione negativa o non intera, documento troncato, schema futuro con campi ignorati solo nell'intestazione, schema v0 riconosciuto solo con il documento esatto, documenti non oggetto rifiutati, schema v0 formattato con spazio bianco, trasformazione v0 → v1 a revisione 1, overflow della revisione rifiutato), 17 sulla scrittura (serializzazione esatta, riconoscimento del namespace dei temporanei, collisione dei nomi fino a 16 tentativi e pulizia del temporaneo non pubblicato, guasti simulati nei quattro passi della prima creazione, commit della prima creazione senza sincronizzazione della directory, guasti simulati negli otto passi del salvataggio, commit del salvataggio senza sincronizzazione, guasti simulati negli otto passi della migrazione dal principale con sorgente intatta e nuovo tentativo riuscito, guasti simulati nei nove passi della migrazione dal backup che non perdono mai il principale corrotto, commit della migrazione senza sincronizzazione, recupero con principale corrotto e conservazione della copia, recupero con principale assente senza copia, guasti simulati nei cinque passi del recupero con nuovo tentativo riuscito, commit del recupero senza sincronizzazione, principale comparso durante la prima creazione mai sovrascritto, temporanei residui ignorati e scartati, nomi simili fuori dal namespace conservati) e 6 di arresto in processi figli;

- 52 test di integrazione su file reali in una radice temporanea creata dalle primitive verificate: i 24 della classificazione e 28 di persistenza (prima esecuzione a revisione 0 e riapertura senza scritture, salvataggi successivi con backup corretto e modalità 0600, lettore concorrente che vede solo generazioni complete durante 20 salvataggi, recupero con principale corrotto e copia conservata, recupero con principale assente, rifiuti senza modifiche e senza cancellazione dei temporanei, overflow della revisione, temporanei del namespace rimossi e altri file intatti, errore di scrittura su radice non scrivibile con generazione precedente intatta, protezioni alterate dopo l'apertura che rifiutano il salvataggio senza riparazione, rivalidazione che rifiuta principale o backup con schema futuro, backup con modalità 0644, principale troncato, revisione divergente e principale scomparso, prima esecuzione con componenti assenti che può ancora salvare). Alla sezione delle migrazioni appartengono: principale v0 migrato con byte originali nel backup e riapertura senza seconda migrazione, backup v0 migrato con principale assente, principale corrotto conservato e backup v0 migrato, documenti v0 non migrabili trattati come corrotti o recuperati da un v1 valido, documento v0 oltre il limite di dimensione, principale sostituito con schema v0 che rifiuta il salvataggio, principale o backup non oggetto (`[0]`, `[0,0]`, `[1,0]`, `[2]`, `[]`, `0`, `null`) mai migrati, con recupero dal backup valido quando esiste e senza scritture quando non esiste, principale v0 formattato ancora migrato con i byte originali conservati. Tutte aprono lo `Store` con `platform::LockedRoot`; le prove dei rifiuti acquisiscono esplicitamente il lock prima della chiamata e confrontano lo stato della radice ignorando soltanto `state.lock`;
- 18 funzioni di prova del lock in `tests/lock.rs` (16 eseguite su macOS, un helper di processo figlio ignorato e una prova del tipo errato compilata solo per Windows): prima acquisizione con creazione 0600, rilascio senza cancellazione né sostituzione, nuova acquisizione sullo stesso file, seconda acquisizione nello stesso processo rifiutata, contenuto arbitrario irrilevante, seconda istanza reale rifiutata con configurazione, backup, note e temporaneo intatti e poi riaperti, radici diverse indipendenti, radice bloccata usata dallo `Store` con la radice libera lasciata identica (nessuna classificazione, pulizia o scrittura), lock legato alla directory ancorata e non al percorso nominale, directory e modalità 0644 rifiutate senza riparazione, collegamento simbolico rifiutato e non seguito, file preesistente riusato senza riscrittura, lock vivo per tutta la durata dello `Store` e morte del detentore con SIGABRT, file di lock conservato e nuova istanza che riapre e salva, migrazione eseguita sotto il lock che esclude una seconda istanza e lascia lo stato invariato durante la migrazione;
- la prova di libreria `the_lock_is_released_only_when_the_last_reference_is_closed` riproduce in modo deterministico la semantica di rilascio: un descrittore duplicato, come quello che un processo forchettato eredita prima di `exec`, tiene il lock anche dopo la chiusura del guard e lo rilascia soltanto alla chiusura dell'ultimo riferimento. Le prove che avviano processi figli coordinano l'avvio con le transizioni del lock, senza disattivare il parallelismo né aggiungere attese;
- i sei arresti di `config` e l'arresto del lock usano processi figli che terminano con `std::process::abort()`, senza unwinding: temporaneo incompleto prima della pubblicazione, backup già sostituito con principale ancora precedente, principale sostituito prima della sincronizzazione finale, copia del principale corrotto conservata prima della pubblicazione del recupero, migrazione non ancora pubblicata con principale v0 intatto e temporaneo scartabile, migrazione già pubblicata con principale v1 e backup v0, detentore del lock vivo al momento dell'arresto. Il padre attende sempre la terminazione con un termine massimo e la pulizia della directory resta al `Drop` del processo di prova; il codice di uscita 101 è rifiutato esplicitamente come panic unwindato. Dopo ogni arresto una nuova istanza riapre lo stato e verifica contenuti, revisioni, backup, copie e temporanei;
- guasti e arresti sono punti di controllo interni, attivi soltanto nella compilazione dei test e isolati per thread; nessun comando o opzione del binario li espone;
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`, `cargo test --workspace --locked --offline` e `cargo build --workspace --locked --offline`: conformi;
- copia autonoma senza `.git` e `target`: build e prove d'integrazione `config`, `lock` e `platform` verdi con la cache locale;
- il crate, comprese le prove d'integrazione `config`, `lock` e `tests/platform_windows.rs`, compila per `x86_64-pc-windows-gnu` con `clippy --all-targets` e warning come errori; le prove di stato sono assegnate al job `windows-non-elevated` con un account effimero non amministratore, che nella CI del 27 novembre 2026 si è fermato prima delle prove, quindi non sono state eseguite.

Le prove di integrazione della configurazione sono comuni a Unix e Windows; solo collegamenti simbolici, modalità POSIX, radice non scrivibile e protezioni alterate restano specifici di Unix. Le prove del lock comuni a Unix e Windows sono acquisizione, rifiuto della seconda istanza, indipendenza delle radici, associazione fra radice e lock, riuso del file e contenuto irrilevante; il tipo errato è provato su entrambe le famiglie (solo compilato su Windows), mentre modalità POSIX e collegamento simbolico sono specifici di Unix. Nessuna prova è stata eseguita su Windows o Linux. La matrice `rust` non esegue le prove di stato su Windows, dove il runner è amministratore: il job separato `windows-non-elevated` le assegna a un account effimero non amministratore con un token verificato prima dell'esecuzione; nella CI del 27 novembre 2026 si è fermato prima delle prove. Nella stessa CI il passo `Test` su Linux non è partito per un errore di compilazione corretto ma non rieseguito.

## Prove non eseguite

- Esecuzione su Windows: prima creazione, salvataggi, recupero, pulizia con `FindFirstFileW`, rinomina e assenza. Le prove compilano per `x86_64-pc-windows-gnu`, ma nessuna è stata eseguita; la durabilità della voce di directory non è disponibile e resta non dichiarata.
- Esecuzione su Linux: nessuna prova eseguita; le prove Unix sono state eseguite solo su macOS e nella CI del 27 novembre 2026 il passo `Test` su Linux non è partito.
- Durabilità alla perdita di alimentazione: `F_FULLFSYNC` e `fsync` della directory vengono eseguiti dalle prove su macOS, ma nessuna prova simula un'interruzione dell'alimentazione e nessuna dichiara verificata la durabilità.
- Arresto del processo: i quattro scenari sono eseguiti su macOS con `std::process::abort()` in processi figli, non con un arresto del sistema operativo o una perdita di alimentazione. Non coprono un arresto durante la scrittura del file, più cicli di salvataggio (T10) né l'esecuzione su Windows o Linux; nessuna prova dichiara la durabilità alla perdita di alimentazione.
- Concorrenza fra processi: T18 è provato con un lettore concorrente nello stesso processo e con due istanze reali su macOS, in cui la seconda è rifiutata con `AlreadyRunning` e lascia intatti configurazione, backup e temporanei. Non è stata eseguita una prova con un secondo scrittore che ignora il lock, né su Windows o Linux.
- Lock con `state.lock` posseduto da un altro utente: la prova richiede privilegi e su questa macchina (uid 501) è stata saltata; l'isolamento fra due account reali non è stato provato.
- Isolamento fra account reali e prove TB-08: eseguito nella CI Linux del 27 novembre 2026 (run 46010768568) con due account effimeri, dinieghi di accesso e pulizia osservati.
- Job di GitHub Actions: la CI è partita il 27 novembre 2026 sul commit `473ab73` (run 46010768568). Il job `windows-non-elevated`, con account effimero limitato al runner GitHub-hosted, verifica di `TOKEN_ELEVATION` e del SID, timeout, pulizia e `--skip real_backend`, si è fermato prima delle prove di stato: la configurazione su Windows non è dimostrata.
- Migrazione reale da una versione distribuita: nessuna versione di Totum ha mai scritto uno schema precedente. Il caso v0 è esclusivamente sintetico e prova il meccanismo; non è una compatibilità storica.
- Migrazione su Windows e Linux: non eseguita; le prove compilano per `x86_64-pc-windows-gnu` ma sono state eseguite soltanto su macOS.

## Limiti

- **Durabilità non provata.** Le garanzie dichiarate sono quelle delle API usate. Su macOS la durabilità richiede `F_FULLFSYNC` e `fsync` della directory; su Linux dipende dal file system; su Windows la voce di directory non è mai sincronizzata e l'esito è sempre `CommittedNotDurable`. Nessuna prova simula la perdita di alimentazione.
- **Un solo scrittore cooperante.** Lo `Store` si costruisce soltanto con `platform::LockedRoot` e lo trattiene per tutta la propria vita; la seconda istanza cooperante è rifiutata con `AlreadyRunning` prima di leggere o scrivere. Il lock appartiene alla directory ancorata che ha prodotto l'acquisizione: non esiste un lock separato da passare a un'altra radice, nemmeno con lo stesso percorso nominale. Il lock è consultivo: non protegge da software dello stesso account che lo ignora, che può modificare i file fra la rivalidazione e la sostituzione. Ogni salvataggio rivalida comunque i due file e rifiuta una revisione diversa da quella caricata, ma la revisione non è un contatore di sicurezza. Il rilascio segue la open file description: un processo forchettato che eredita il descrittore prima di `exec` può trattenerlo per quella breve finestra; il daemon non crea processi.
- **Salvataggio diverso dal recupero.** Un principale assente o corrotto con backup valido non viene mai riparato da `save`: il salvataggio restituisce `RecoveryRequired` e il recupero resta affidato a una nuova `Store::open`.
- **Rilevamento della divergenza.** La revisione divergente viene rilevata solo confrontando il principale su disco con quella caricata; una coppia principale/backup modificata insieme e coerente con sé non è distinguibile da una generazione legittima, perché il lock coordina soltanto le istanze che lo rispettano.
- **Backup dopo un errore.** Se il salvataggio fallisce nella sostituzione del principale, `config.json.prev` può essere già stato aggiornato alla generazione del principale. La coppia resta valida, ma il backup non è più la generazione precedente fino al salvataggio successivo.
- **Copie del principale corrotto.** `config.corrupt.*` viene scritto prima del recupero e non viene mai rimosso dal modulo: non esiste ancora una politica di conservazione o di pulizia. Un recupero ripetuto dopo un arresto può lasciare più copie dello stesso principale.
- **Namespace dei temporanei.** La pulizia rimuove ogni nome `.config.<pid>.<contatore>.tmp`, indipendentemente dal PID, perché presuppone che nessun'altra istanza cooperante stia scrivendo: il lock esclusivo garantisce questa condizione per le istanze di Totum. Un processo che ignora il lock e scrivesse temporanei nella stessa radice vedrebbe i propri file rimossi da un'apertura. I nomi fuori dal namespace non vengono mai toccati.
- Lo schema v1 non contiene campi di dominio; quando arriveranno, completare la prima esecuzione quando manca solo la configurazione potrebbe non essere più sicuro e richiederà una decisione.
- **Migrazioni solo in avanti, formato sintetico.** Lo schema v0 non è un formato storico: esiste soltanto per provare il meccanismo e non sarà mai prodotto da una versione distribuita. Non esiste una migrazione all'indietro, un downgrade o una reinterpretazione di uno schema futuro, e una generazione v0 con qualunque contenuto diverso da `{"schemaVersion":0}` è corrotto e non viene mai migrata. Il meccanismo ammette un solo salto sintetico; quando esisterà uno schema reale, la catena di trasformazioni e le relative regole andranno definite e provate con una decisione registrata.
- **Migrazione e backup.** Migrando dal principale, `config.json.prev` riceve i byte v0 originali; migrando dal backup, il backup riceve i propri byte v0 e il principale corrotto viene copiato in `config.corrupt.*`. Il backup v0 resta riservato accanto a un principale v1 valido e non viene migrato finché il principale resta valido; una copia conservata in un tentativo interrotto può restare come diagnostica e non viene mai rimossa dal modulo.
- La semantica dei file di identità e registro è un input del chiamante; dal passo P7 `identity::classify` produce gli stati e `identity::initialize` completa soltanto i file iniziali mancanti, sotto lo stesso lock ma con accesso esclusivo `&mut`, senza creare la configurazione. Gli stati non iniziali restano rifiutati: il modulo `identity` non li azzera né li corregge.
- Gli errori del sistema sui file sono riportati con percorso e causa; nessun messaggio contiene segreti, che il formato non ammette.
