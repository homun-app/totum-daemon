# Decisioni tecniche del daemon

Le sezioni registrano il contesto delle singole decisioni; per le evidenze di esecuzione correnti, inclusi i limiti superati dalle CI successive, vedere [ci-verification.md](ci-verification.md).


Registro pubblico delle decisioni che riguardano questo repository. Le decisioni valgono per gli incrementi D1 e D2 e per i passi da P1 a P9 di D3 e per P10-A; non fissano il futuro protocollo del prodotto.

## AD-001 — Codifica JSON UTF-8 per i contratti di lavoro A/C

**Data:** 26 novembre 2026. **Stato:** accettata per D2. **Ambito:** superfici A e C del crate `totum-contract`.

**Contesto.** Serviva una codifica semplice e motivata per envelope, payload, fixture e test, senza introdurre un protocollo stabile né vincolare l'app mobile o il relay. La specifica D2 richiede fixture leggibili e rivedibili, errori di decodifica distinti dagli errori applicativi e nessuna dipendenza non necessaria.

**Alternative valutate.**

- **CBOR** — binario compatto e standardizzato, ma fixture illeggibili senza strumenti e maggiore superficie di decodifica da verificare.
- **MessagePack** — compatto, ma stessa difficoltà di revisione e nessun vantaggio richiesto dagli obiettivi di D2.
- **Schema-first (Protobuf o simile)** — generazione di codice e toolchain aggiuntiva, contraria al perimetro di lavoro minimo e alla scelta di non fissare schemi stabili.
- **Formato binario artigianale** — massimo controllo, ma errori di parsing e manutenzione senza alcun beneficio dimostrato in questa fase.
- **JSON UTF-8, un documento per messaggio** — scelto: testuale, ispezionabile, verificabile con fixture statiche, senza generazione di codice.

**Decisione.** I messaggi A/C sono oggetti JSON UTF-8, uno per messaggio, con campi dell'envelope e del payload allo stesso livello. `serde` e `serde_json` realizzano serializzazione e parsing; i limiti di lunghezza e le regole di validazione sono espliciti nel crate.

**Conseguenze.**

- Le fixture sono leggibili e i test verificano valori decodificati, non soltanto round-trip.
- La rappresentazione è più grande di un formato binario: dimensione e prestazioni non sono state misurate e non sono un obiettivo di D2.
- I campi sconosciuti sono ignorati per le aggiunte minori; i tipi sconosciuti sono rifiutati.
- La decisione **non** fissa la codifica del futuro protocollo mobile o del relay, né un formato stabile di schemi o client.

**Revisione.** Da rivedere quando esistono misurazioni reali o requisiti di trasporto che giustifichino un formato binario.

## AD-002 — Ammissione di `serde` e `serde_json`

**Data:** 26 novembre 2026. **Stato:** accettata per D2. **Ambito:** dipendenze del workspace.

**Contesto.** La regola del repository preferisce la libreria standard e ammette dipendenze esterne soltanto se motivate, registrate e provenienti da fonti pubbliche. La codifica JSON richiede parsing e serializzazione robusti; scriverli a mano sarebbe una superficie di errore non giustificata.

**Decisione.** Sono ammesse `serde` (con la feature `derive`) e `serde_json`, dal registro pubblico di crates.io, insieme al loro albero:

`itoa`, `memchr`, `proc-macro2`, `quote`, `serde_core`, `serde_derive`, `syn`, `unicode-ident`, `zmij`.

Nessuna feature `preserve_order`: senza di essa le mappe JSON usano un ordine stabile per chiave. Non sono ammesse dipendenze di rete, crittografia o runtime asincrono. Lo stesso albero è ammesso come dipendenza di sviluppo di `totum-daemon`, dove serve soltanto al test di perimetro per leggere l'output di `cargo metadata`: non entra nei binari.

**Controlli.**

- `Cargo.lock` è incluso e build e test usano `--locked`.
- Il test `dependency_perimeter` interroga `cargo metadata` e accetta soltanto i pacchetti del workspace e i pacchetti approvati provenienti dal registro pubblico di crates.io. Verifica l'appartenenza al workspace, la posizione canonica dei pacchetti locali, i tipi di dipendenza normale, di sviluppo e di build e le sostituzioni di sorgente pertinenti; una crate locale fuori dal checkout fallisce anche se il nome è ammesso, e sorgenti Git, registri autenticati o privati falliscono. Il lockfile è controllato anche separatamente, perché da solo non distingue una crate del workspace da una copia esterna con lo stesso nome.
- Il workspace compila da un clone autonomo, senza credenziali.

**Alternative scartate.** Un parser JSON scritto a mano o una crate minima alternativa: più codice da verificare, stessa superficie di parsing e nessun vantaggio per il perimetro D2.

## AD-003 — Nome provvisorio del crate dei contratti

**Data:** 26 novembre 2026. **Stato:** accettata per D2. **Ambito:** struttura del workspace.

**Contesto.** Il crate pubblico dei contratti minimi ha bisogno di un nome coerente con la struttura proposta, senza impegnarlo come nome definitivo del protocollo.

**Decisione.** Il crate si chiama `totum-contract` ed è membro del workspace, con `publish = false`. Il nome e la forma dei tipi sono provvisori: i contratti sono strumenti di lavoro, non un protocollo stabile.

**Conseguenze.** Un'eventuale rinomina o ristrutturazione è una modifica coordinata di workspace, documentazione pubblica e riferimenti interni, e non richiede compatibilità con client esistenti, perché non ne esistono.

**Revisione.** Prima di qualunque pubblicazione di pacchetti o di un protocollo stabile.

## AD-004 — Dipendenze di piattaforma per D3 P1/P2 e lettura degli ACL

**Data:** 26 novembre 2026. **Stato:** accettata per D3 P1/P2; estesa il 27 novembre 2026 alle primitive di scrittura di P4, senza nuove dipendenze. **Ambito:** dipendenze del crate `totum-daemon` e lettura degli ACL effettivi.

**Contesto.** Il modulo `platform` deve verificare proprietario, tipo e protezioni di radice e antenati, creare directory con permessi imposti alla creazione, rifiutare collegamenti e reparse point e ancorare le operazioni. La libreria standard non offre né l'identificativo effettivo con le varianti `*at` in forma portabile, né il rilevamento del token elevato, né la creazione di directory con un descrittore di sicurezza esplicito. Gli ACL estesi di macOS non sono coperti da `rustix`.

**Decisione.** Sono ammesse, dal solo registro pubblico di crates.io, con feature ristrette e `publish = false`:

- `rustix` 1.1.6, feature `std`, `fs`, `process`, come dipendenza normale su Unix e come dipendenza di sviluppo su Unix: fornisce `openat`, `mkdirat`, `fstatat`, `fchmod`, `fgetxattr` e l'identificativo effettivo;
- `windows-sys` 0.61.2, feature `Win32_Foundation`, `Win32_Security`, `Win32_Security_Authorization`, `Win32_Storage_FileSystem`, `Win32_System_Threading`, come dipendenza normale su Windows: fornisce token, descrittori di sicurezza, DACL, `CreateDirectoryW`, `CreateFileW` e le informazioni sull'handle;
- l'albero transitivo risolto dal lockfile: `bitflags`, `errno`, `libc`, `linux-raw-sys` dietro `rustix`, e `windows-link` dietro `windows-sys`;
- `serde` e `serde_json` restano dipendenze di sviluppo di `totum-daemon`, come in D2.

Gli ACL estesi di macOS sono letti con dichiarazioni `extern "C"` verso libSystem (`acl_get_fd_np`, `acl_get_entry`, `acl_get_permset_mask_np`, `acl_get_qualifier`, `mbr_uuid_to_id`, `acl_free`): nessuna crate aggiuntiva. Gli ACL POSIX di Linux sono analizzati direttamente dall'attributo `system.posix_acl_access`, con un parser puro provato su byte sintetici anche su macOS.

**Alternative scartate.**

- `exacl` e `posix-acl`: aggiungono una dipendenza e una superficie da verificare per codice comunque specifico di piattaforma; il parser richiesto è breve e verificabile.
- La crate `windows` completa: superficie e tempi di compilazione maggiori delle sole funzioni necessarie.
- Un parser JSON o un wrapper ACL scritto a mano senza dipendenze: perderebbe `openat`, l'identificativo effettivo e il token elevato, oppure richiederebbe FFI non verificate su ogni sistema.
- `tempfile`, `dirs`, `fs4`: non necessari in P1/P2; le directory di prova sono gestite dai test.

**Controlli.**

- `Cargo.lock` è incluso; build, test e `clippy` usano `--locked --offline`.
- Il test `dependency_perimeter` ammette i 18 pacchetti esterni approvati e verifica, per ogni dipendenza diretta dei pacchetti del workspace, la coppia e i tipi effettivi (normale, sviluppo, build); per le dipendenze fra pacchetti esterni verifica la coppia. Una crate non elencata, una coppia nuova o un tipo di dipendenza diverso falliscono il controllo. Il lockfile è verificato separatamente per sorgenti Git, percorsi e registri diversi da crates.io.
- `rustix` e `windows-sys` non entrano nei binari: sono usati soltanto dai moduli `platform` e `config`, che nessun percorso operativo chiama in P1/P2. Dal passo P4 il modulo `platform` usa le stesse feature per pubblicare un temporaneo senza sostituzione, sostituire una voce, rimuoverla, elencare la radice e sincronizzare file e directory (`renameat_with`, `renameat`, `unlinkat`, `readdir`, `fsync`, `fcntl_fullfsync` su Unix; `MoveFileExW`, `DeleteFileW`, `FindFirstFileW`, `FlushFileBuffers` su Windows).
- La toolchain resta 1.95.0; nessun MSRV è dichiarato.

**Limiti.** Il codice Windows è compilato per `x86_64-pc-windows-gnu` ma non eseguito. Il parser POSIX non è stato eseguito su Linux. Le chiamate libSystem usate sono quelle documentate per gli ACL estesi con maschera di permessi; un errore di lettura o un formato sconosciuto producono `UnverifiableProtection`, mai un permesso implicito.

**Revisione.** La radice è collegata all'avvio da P8 (AD-010). Da rivedere quando una prova reale su Windows o Linux richiederà API diverse.

## AD-005 — `serde` e `serde_json` come dipendenze normali per la configurazione

**Data:** 26 novembre 2026. **Stato:** accettata per D3 P3. **Ambito:** dipendenze del crate `totum-daemon`.

**Contesto.** Il passo P3 introduce il modulo `config` come libreria testabile: schema JSON v1, validazione rigorosa, riconoscimento dello schema futuro e classificazione in sola lettura di `config.json` e `config.json.prev`. La regola del repository preferisce la libreria standard e ammette dipendenze esterne soltanto se motivate, registrate e provenienti da fonti pubbliche. Fino a P1/P2 `serde` e `serde_json` erano dipendenze soltanto di sviluppo di `totum-daemon`, usate dal test di perimetro.

**Decisione.** `serde` con la feature `derive` e `serde_json`, dal registro pubblico di crates.io e con l'albero già ammesso da AD-002, diventano dipendenze normali di `totum-daemon`. Realizzano parsing e validazione della configurazione versionata: campi sconosciuti e duplicati rifiutati, tipi controllati, distinzione fra documento corrotto e schema futuro. Dal passo P8 entrano nel binario `totum-daemon` attraverso il percorso operativo, che apre lo `Store` sotto il lock; `totum` non li usa.

**Alternative scartate.**

- Un parser JSON scritto a mano: stessa superficie di parsing da verificare, senza alcun vantaggio e con errori più probabili.
- Un formato binario o TOML con una nuova dipendenza: fixture illeggibili o dipendenza aggiuntiva senza un requisito che le giustifichi.
- Validare i campi con `serde_json::Value` dopo il parsing: perderebbe il rifiuto dei campi duplicati e dei tipi, che è richiesto da P3.

**Conseguenze.**

- `Cargo.lock` non cambia contenuto: le due crate erano già risolte; cambia soltanto il tipo di dipendenza dichiarato per `totum-daemon`.
- Il test `dependency_perimeter` ammette per `totum-daemon` le coppie `serde` e `serde_json` come normali; `rustix` resta normale e di sviluppo su Unix, `windows-sys` normale su Windows. Ogni altra coppia o tipo fallisce.
- Restano valide le regole di AD-002: nessuna feature `preserve_order`, nessuna dipendenza di rete, crittografia o runtime asincrono, nessun albero fuori dal registro pubblico.
- La toolchain resta 1.95.0 e `publish = false` non cambia.

**Limiti.** Il codice di configurazione è eseguito soltanto su macOS; Windows e Linux non sono stati provati. La decisione non ammette altre dipendenze per il daemon.

**Revisione.** Aggiornata: il modulo `config` è collegato all'avvio da P8 (AD-010). Da rivedere quando i campi di dominio richiederanno un formato diverso da JSON.

## AD-006 — Scrittura, backup e durabilità della configurazione

**Data:** 27 novembre 2026. **Stato:** accettata per D3 P4. **Ambito:** persistenza del modulo `config` e primitive di `platform` che la sostengono.

**Contesto.** P4 deve creare la configurazione a revisione 0, salvare le generazioni successive con un backup, sostituire il principale in modo atomico, recuperare dal backup e conservare il principale corrotto. Il piano D3 richiede che dopo la prima creazione `config.json` non sia mai assente, che un lettore veda sempre una generazione completa, che un errore prima del commit non modifichi il principale e che un errore dopo il commit non venga presentato come salvataggio fallito. Le API della libreria standard non offrono la pubblicazione senza sostituzione né la sincronizzazione di una voce di directory; `rustix` e `windows-sys` sono già ammessi con le feature necessarie (AD-004).

**Decisione.**

- Ogni generazione viene scritta in `.config.<pid>.<contatore>.tmp`, creato in modo esclusivo con modalità 0600, sincronizzato e poi rinominato. Il nome è l'unico riconosciuto dalla pulizia.
- Prima creazione: il temporaneo viene pubblicato su `config.json` soltanto se la destinazione non esiste (`renameat2`/`renameatx_np` con `RENAME_NOREPLACE`/`RENAME_EXCL`, `MoveFileExW` senza `MOVEFILE_REPLACE_EXISTING`). Una destinazione comparsa produce `AppearedDuringFirstRun` e nessuna sovrascrittura.
- Salvataggio: il backup viene **preparato per copia** in un secondo temporaneo e rinominato su `config.json.prev`; il principale non viene mai spostato. La sostituzione del principale con il temporaneo nuovo è il punto di commit.
- Recupero: il principale corrotto viene copiato in `config.corrupt.<epoch>.<contatore>` e lasciato al suo posto; i byte del backup vengono scritti in un temporaneo e sostituiscono il principale. `config.json.prev` resta la generazione recuperata e la revisione è quella del backup.
- Durabilità: `F_FULLFSYNC` su macOS, `fsync` su Linux, `FlushFileBuffers` su Windows; la directory viene sincronizzata solo su Unix. Su Windows `sync_directory` dichiara `Unsupported` e il commit è `CommittedNotDurable`.
- Un errore prima del commit è `Err` e garantisce che il principale è la generazione precedente. Un errore dopo il commit è `CommittedNotDurable`: il salvataggio è riuscito, la revisione avanza e non esiste un nuovo tentativo automatico.
- La pulizia rimuove soltanto i nomi `.config.<pid>.<contatore>.tmp`; ogni altro oggetto resta intatto.

**Alternative scartate.**

- Scrivere direttamente il principale: un arresto o un errore lascerebbe un file troncato e distruggerebbe l'unica generazione.
- Spostare il principale su `.prev` e pubblicare il nuovo: fra i due passi il principale sarebbe assente, contro il requisito di T18.
- Usare `fsync` da solo su macOS: non è sufficiente per la durabilità dichiarata; `F_FULLFSYNC` è la primitiva documentata.
- Affidarsi alla sincronizzazione della directory su Windows: nessuna API nella libreria standard o in `windows-sys` con le feature ammesse; la durabilità non viene dichiarata invece di essere simulata.
- Rimuovere o riparare il principale corrotto: la copia `config.corrupt.*` conserva l'evidenza e il principale non scompare in nessun momento.

**Conseguenze.**

- Il commit è osservabile: `Commit::Committed` con durabilità confermata, `Commit::CommittedNotDurable` senza. La revisione in memoria segue il commit.
- Dopo un errore nella sostituzione del principale, `.prev` può essere già stato aggiornato alla stessa generazione del principale: la coppia resta valida e il tentativo successivo la ricalcola.
- Nessuna nuova dipendenza: le funzioni usate appartengono alle feature già ammesse da AD-002 e AD-004.
- Identità e registro sono stati aggiunti da P7 (AD-009) e il collegamento all'avvio da P8 (AD-010). Il lock fra processi è stato introdotto da P5 (AD-007) e le migrazioni in avanti da P6 (AD-008).

**Limiti.** La durabilità non è stata provata con una perdita di alimentazione. Le API Windows sono compilate per `x86_64-pc-windows-gnu` ma non eseguite. `Store` serializza le operazioni della stessa istanza; l'esclusione fra processi cooperanti è aggiunta dal lock di P5 (AD-007), che non protegge da software che ignora il protocollo.

**Revisione.** Aggiornata da AD-007 per il lock di P5, da AD-008 per le migrazioni di P6 e da AD-010 per il collegamento all'avvio; da rivedere quando una prova reale su Windows o Linux richiederà API diverse.

## AD-007 — Lock esclusivo consultivo per radice di stato

**Data:** 27 novembre 2026. **Stato:** accettata per D3 P5. **Ambito:** lock di istanza unica del modulo `platform` e costruzione di `config::Store`.

**Contesto.** P5 deve garantire che due istanze di Totum sulla stessa radice non leggano, classifichino o scrivano contemporaneamente lo stato. Il lock deve essere rilasciato anche alla morte del processo, non deve dipendere dal contenuto di un file o da un PID e non deve cancellare o sostituire il file al rilascio. La libreria standard di Rust 1.95 offre `File::try_lock` (stabile dalla 1.89): su Unix `flock` con `LOCK_EX | LOCK_NB`, su Windows `LockFileEx` con `LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY`. Il rilascio avviene alla chiusura del descrittore o alla terminazione del processo. Non serve alcuna nuova dipendenza.

**Decisione.**

- Il lock è il `flock`/`LockFileEx` esclusivo di `state.lock` nella radice verificata, acquisito con `platform::lock(VerifiedRoot)` senza attesa. `lock` consuma la radice e restituisce `LockedRoot`; un secondo detentore produce `PlatformError::AlreadyRunning`; nessun timeout, nessuna rimozione e nessuna interpretazione del contenuto o di un PID.
- `platform::LockedRoot` possiede insieme la directory ancorata e il descrittore del lock: il lock resta esclusivo finché l'oggetto è vivo e viene rilasciato alla chiusura dell'ultima open file description o alla morte del processo. L'identità è la directory ancorata (descrittore e inode su Unix, volume e indice del file su Windows), non il percorso nominale. Il rilascio non cancella né sostituisce `state.lock`; una nuova acquisizione riapre lo stesso file.
- Creazione e apertura del file usano le primitive verificate: su Unix `O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW` con modalità 0600 imposta, oppure apertura di un file esistente con `O_NOFOLLOW`, verifica di tipo regolare, proprietario, modalità e ACL, senza riparazione; su Windows `CreateFileW` con `CREATE_NEW`, condivisione completa per consentire alla seconda istanza di raggiungere `LockFileEx`, riapertura con `FILE_FLAG_OPEN_REPARSE_POINT` e verifica della DACL protetta. Un oggetto preesistente non sicuro è un rifiuto.
- `config::Store::open` richiede `LockedRoot` e lo trattiene per tutta la vita dello `Store`: classificazione operativa, pulizia dei temporanei e scritture avvengono soltanto sulla radice posseduta insieme al lock, e non esiste un percorso che separi il guard dalla radice o lo rilasci mentre lo `Store` resta utilizzabile. La classificazione in sola lettura di P3 resta separata e non scrive.
- La rivalidazione prima dei salvataggi resta: il lock coordina le istanze cooperanti, la rivalidazione rileva uno stato modificato da altri mezzi.

**Alternative scartate.**

- **File di lock per sola esistenza** (`O_CREAT | O_EXCL` come lock): un arresto lascerebbe il file e richiederebbe di interpretare PID o contenuto per distinguere un residuo da un detentore, esattamente ciò che il piano vieta.
- **File di PID con controllo `kill(pid, 0)`**: richiede di interpretare il contenuto, ha ambiguità di riuso del PID e non è portabile su Windows.
- **Timeout e rimozione dei presunti lock scaduti**: distruggerebbe il lock di un processo vivo lento o sospeso.
- **Solo rivalidazione senza lock**: lascia aperta la finestra fra rivalidazione e sostituzione del principale, già dichiarata in AD-006.
- **Crate `fs4`, `fd-lock` o simili**: superficie aggiuntiva per una funzionalità già offerta dalla libreria standard.

**Conseguenze.**

- Nessuna nuova dipendenza e nessuna modifica a `Cargo.lock`; il lock usa la libreria standard.
- `LockedRoot` e `Store` hanno ownership accoppiata: la radice e il lock sono lo stesso oggetto, quindi non si può classificare, pulire o scrivere su una radice diversa da quella bloccata, nemmeno con lo stesso percorso nominale, e il lock non può essere rilasciato mentre lo `Store` scrive, perché è un suo campo.
- Una seconda istanza può preparare e verificare il contenitore, ma prima dell'acquisizione non legge né modifica configurazione, backup o temporanei; l'esito è `AlreadyRunning`.
- `state.lock` resta nella radice dopo il rilascio: è un file vuoto o con contenuto arbitrario mai letto, creato 0600 o con DACL protetta e verificato a ogni acquisizione.
- Il rilascio segue la open file description, non il singolo `File`: duplicati con `try_clone` e descrittori ereditati da un processo figlio prima di `exec` trattengono il lock finché l'ultima copia non viene chiusa. Nei test il processo figlio forchettato crea una finestra transitoria; le prove coordinano l'avvio dei figli e le riacquisizioni con un mutex dedicato, senza serializzare l'intera suite, e la semantica è fissata dalla prova di libreria `the_lock_is_released_only_when_the_last_reference_is_closed`. Il daemon non crea processi, quindi la finestra non esiste nel percorso operativo.

**Limiti.** Il lock è consultivo, non obbligatorio: coordina le istanze di Totum che rispettano il protocollo, non software dello stesso account che lo ignora o che opera direttamente sui file. Il comportamento è eseguito su macOS; su Windows e Linux le prove del lock sono compilate ma non eseguite. Il caso di un `state.lock` posseduto da un altro utente è provato soltanto dove il cambio di proprietario è consentito (richiede privilegi) e non è stato eseguito su questa macchina. L'isolamento fra account reali è stato osservato soltanto dalla CI Linux (TB-08).

**Revisione.** Il lock è collegato all'avvio del processo da P8 (AD-010). Da rivedere quando una prova reale su Windows o Linux richiederà API diverse.

## AD-008 — Migrazioni della configurazione soltanto in avanti con schema v0 sintetico

**Data:** 27 novembre 2026. **Stato:** accettata per D3 P6. **Ambito:** riconoscimento di una generazione con schema precedente, trasformazione verso lo schema v1 e persistenza sotto `LockedRoot`.

**Contesto.** P6 deve migrare una configurazione con schema precedente verso lo schema v1, conservando la generazione precedente nel backup e applicando la stessa semantica di commit di P4. Nessuna versione distribuita di Totum ha mai scritto uno schema precedente: senza un formato di prova il meccanismo non sarebbe verificabile, e un formato inventato non deve diventare una compatibilità storica implicita. La migrazione deve essere soltanto in avanti, mai un downgrade, e non può rendere ambigua la provenienza quando principale e backup hanno schemi diversi.

**Decisione.**

- Lo schema **v0 è esclusivamente sintetico** e non rappresenta un formato storico: è il documento esatto `{"schemaVersion":0}`, senza `revision` e senza altri campi. Qualsiasi altro contenuto con `schemaVersion` inferiore a 1 è corrotto e non viene mai migrato.
- Un documento di configurazione è sempre un **oggetto JSON**: array, scalari e `null` sono corrotti, mai una v1 né una sorgente migrabile. La forma è imposta prima di interpretare lo schema leggendo il primo carattere dopo lo spazio JSON bianco, senza un valore intermedio, quindi i campi duplicati restano visibili alla deserializzazione e vengono rifiutati.
- La revisione di uno schema v0 è implicitamente 0. La trasformazione pura `migrate_v0_to_v1` produce `{"schemaVersion":1,"revision":1}\n` con un incremento controllato: un overflow produce `MigrationRevisionOverflow` prima di ogni scrittura. Senza `revision` la revisione migrata non può collidere con o superare un backup v1 valido.
- La classificazione distingue la provenienza della generazione da migrare. Un principale v0 produce `MigrationRequired { source: Main }` quando il backup è assente, corrotto o v0; un backup v0 produce `MigrationRequired { source: Backup }` soltanto quando il principale è assente o corrotto. Un backup v0 accanto a un principale v1 valido resta riservato (`OlderSchema`) e non viene migrato. Un principale v0 accanto a un backup v1 valido produce `SchemaRegression`: migrare il principale scarterebbe una generazione più nuova.
- La persistenza riusa l'ordine di P4 sotto lo stesso `LockedRoot`: temporaneo della nuova generazione v1 creato, scritto e sincronizzato; secondo temporaneo con i **byte originali** della sorgente, sincronizzato e rinominato su `config.json.prev`; sostituzione atomica del principale come punto di commit; sincronizzazione della directory. Se la sorgente è il backup con un principale corrotto, il principale viene prima copiato in `config.corrupt.<epoch>.<contatore>` senza essere spostato. La sorgente non viene mai sovrascritta prima che la nuova generazione sia scritta e sincronizzata.
- Un errore prima del commit è un'operazione non commessa: la generazione sorgente e i file necessari al recupero restano intatti e i temporanei vengono rimossi. Un errore dopo il commit è `CommittedNotDurable`, non un errore di migrazione, e non esiste un nuovo tentativo automatico.
- `Store::save` non applica mai una migrazione: se la rivalidazione trova una generazione con schema precedente restituisce `MigrationRequired`. Una migrazione parte soltanto da `Store::open` dopo una decisione che la ammette. `MigrationSourceChanged` resta un rifiuto esplicito se la sorgente non è più quella classificata, anche se sotto un unico lock il caso non è raggiungibile.
- La pulizia dei temporanei del namespace riservato avviene prima della migrazione, come per prima creazione, stato normale e recupero.

**Alternative scartate.**

- **Schema v0 con una `revision`:** la revisione migrata (v0 + 1) potrebbe eguagliare o superare quella di un backup v1 valido, rendendo la regressione di schema non rilevabile o la migrazione non additiva. Un v0 senza revisione evita l'ambiguità e basta a provare il meccanismo.
- **Parsare prima in `serde_json::Value` e poi validare:** un valore intermedio perderebbe i campi duplicati e indebolirebbe il rifiuto. La forma si controlla sui byte, prima della deserializzazione, senza costruire valori.
- **Riconoscere qualunque `schemaVersion` inferiore e reinterpretarlo:** un documento sconosciuto verrebbe trasformato senza sapere cosa contiene. Solo il documento sintetico esatto è migrabile.
- **Migrare il backup anche quando il principale v1 è valido:** il backup rappresenta la generazione precedente e una migrazione al suo posto complicherebbe il recupero senza alcun beneficio.
- **Migrare dal principale accanto a un backup più nuovo:** scarterebbe una generazione più nuova; è un rifiuto.
- **Riscrivere la sorgente sul posto:** un errore o un arresto distruggerebbe l'unica copia prima del commit. Il backup riceve i byte originali prima della sostituzione.
- **Migrare durante un salvataggio:** mescolerebbe due decisioni e renderebbe più difficile distinguere un'operazione non commessa da un recupero o da una migrazione implicita.
- **Un comando diagnostico nel binario:** i punti di guasto restano interni alle prove; in P6 nessun percorso operativo era collegato e P8 collega soltanto l'apertura, non comandi di migrazione.

**Conseguenze.**

- Dopo la migrazione la riapertura trova il principale v1 e il backup v0 riservato: non esiste una seconda migrazione e la revisione è quella prodotta dalla trasformazione.
- Il backup conserva la generazione precedente con schema v0 in entrambe le provenienze; migrando dal backup, il principale corrotto resta anche in `config.corrupt.*`.
- Nessuna nuova dipendenza: la trasformazione e la persistenza usano solo `serde_json` già ammesso e le primitive di `platform`.
- Identità e registro sono stati aggiunti da P7 (AD-009) e il collegamento all'avvio da P8 (AD-010); restano fuori una catena di più schemi reali e qualunque migrazione all'indietro.

**Limiti.** Il formato v0 è sintetico e non potrà mai descrivere dati reali; il meccanismo ammette un solo salto. Le prove sono state eseguite su macOS; su Windows e Linux sono compilate ma non eseguite. La durabilità alla perdita di alimentazione non è provata: gli arresti simulati usano `abort` nei processi figli e non equivalgono a una perdita di alimentazione.

**Revisione.** Quando esisterà uno schema reale successivo al v1, quando la catena di migrazioni conterà più passi o quando una prova reale su Windows o Linux richiederà regole diverse.

## AD-009 — Formati minimi e prima inizializzazione di identità e registro

**Data:** 27 novembre 2026. **Stato:** accettata per D3 P7. **Ambito:** formati persistiti di `identity.json` e `controllers.json`, classificazione e completamento della prima inizializzazione.

**Contesto.** P7 deve rappresentare lo stato persistito iniziale dell'identità e il contenitore vuoto del registro dei controller, fornire alla configurazione la coerenza dei due componenti e completare i soli file iniziali mancanti sotto il lock esclusivo. Non sono ammessi chiavi, impronte, token, pairing, autorizzazione, modifica dei grant, revoca, `resetIdentity` né uno schema delle future voci dei controller. Due file separati convivono con `config.json`, che già ha una generazione principale e un backup; il formato dei file di identità deve restare minimo, verificabile e incapace di confondere assenza, contenuto iniziale e contenuto futuro.

**Decisione.**

- **Due documenti separati e versionati.** `identity.json` contiene `{"schemaVersion":1,"state":"notInitialized"}` e `controllers.json` contiene `{"schemaVersion":1,"grantRevision":0,"controllers":[]}`, ciascuno con un solo a capo finale. `schemaVersion` è un intero e identifica il formato; ogni versione diversa da 1 è uno schema non compreso e viene rifiutata. In P7 non esistono migrazioni dei file di identità.
- **Nessuna revisione nei file di identità.** A differenza di `config.json`, non esistono generazioni, backup o recupero propri: la sola transizione ammessa in D3 è assente → iniziale, quindi una revisione non avrebbe un uso verificabile e introdurrebbe una compatibilità non necessaria. Il campo non esiste e non viene riservato.
- **Oggetti JSON con campi esatti.** Un documento deve essere un oggetto prima ancora di essere deserializzato: array, scalari e sequenze posizionali sono rifiutati. Campi sconosciuti, duplicati, mancanti, tipi errati e documenti troncati producono `InvalidDocument`. `controllers` è letto come array di valori JSON non interpretati perché il formato delle voci non esiste: un array non vuoto è un contenuto non iniziale, non un documento valido da correggere.
- **Contenuti non iniziali sono rifiuti.** Uno stato diverso da `notInitialized`, una `grantRevision` diversa da 0 o un registro non vuoto sono validi ma non ammessi in D3: `NonInitial`, nessun azzeramento, nessuna correzione, nessuna riscrittura. Un `controllers.json` corrotto o con schema futuro non diventa assenza e non viene rigenerato.
- **Stati tipizzati per la configurazione.** `identity::classify` produce per ciascun file `Absent`, `Initial`, `NonInitial` o `Unusable`; `Classification::coherence()` restituisce `config::CoherenceInput`. L'assenza resta distinta da un file iniziale presente e l'assenza è riconosciuta soltanto dall'esito dedicato `PlatformError::FileAbsent`. Le letture usano le primitive verificate, il limite di 64 KiB per file e il ricontrollo del percorso, senza seguire collegamenti o reparse point e senza riparare proprietario, modalità o ACL.
- **Prima inizializzazione soltanto sotto `LockedRoot`, con accesso esclusivo.** `identity::initialize` richiede `&mut LockedRoot`, l'oggetto che possiede insieme la radice ancorata e il lock esclusivo: la firma impedisce due chiamate concorrenti sulla stessa radice, quindi la pulizia dei temporanei non può rimuovere il temporaneo attivo di un'altra chiamata; radici diverse restano indipendenti e non esiste un lock globale. Dopo l'inizializzazione lo stesso `LockedRoot` può essere trasferito a `config::Store`. Classifica identità, registro, configurazione e backup prima di qualunque scrittura e completa soltanto i file mancanti: con configurazione e backup assenti e almeno un componente assente crea i file mancanti; con configurazione o backup presenti e un componente assente rifiuta lo stato incoerente; un contenuto non iniziale, inutilizzabile o una configurazione rifiutata bloccano ogni modifica. La configurazione non viene mai creata dal modulo.
- **Ordine e punti di commit per file.** Prima `identity.json`, poi `controllers.json`. Ogni file è scritto in un temporaneo esclusivo `.identity.<pid>.<contatore>.tmp` con modalità 0600, sincronizzato e pubblicato con una rinomina che non sostituisce la destinazione; la pubblicazione è il punto di commit di quel file, la sincronizzazione della directory ne conferma la durabilità e un suo fallimento produce `CommittedNotDurable` senza annullare il commit. La coppia non è atomica: un arresto fra i due file lascia uno stato parziale che il riavvio completa creando soltanto il file mancante, e nessun file iniziale già presente viene riscritto.
- **Nessuna sovrascrittura.** La pubblicazione senza sostituzione è essa stessa la garanzia: se la destinazione compare fra classificazione e rinomina, il file non viene toccato, la classificazione viene ripetuta entro un numero finito di cicli e un conflitto persistente produce `AppearedDuringInitialization`. `Startup::created` elenca i file effettivamente pubblicati e `InitError::created` quelli pubblicati prima di un guasto.
- **Namespace dei temporanei separato e pulizia limitata.** I temporanei di identità usano `.identity.<pid>.<contatore>.tmp` e la pulizia del modulo rimuove soltanto quel namespace: i temporanei `.config.*`, le copie di recupero, i file dell'utente e i nomi simili restano intatti. Il motore di persistenza della configurazione non viene duplicato: il modulo riusa le primitive verificate di `platform` e le stesse regole di pubblicazione e sincronizzazione già fissate per P4, non `Store` né i suoi file.
- **Nessuna dipendenza nuova.** I formati usano `serde` e `serde_json`, già ammessi per la configurazione; scrittura, rinomina, sincronizzazione e lock usano le primitive esistenti di `platform` e la libreria standard.

**Alternative scartate.**

- **Un solo file con identità e registro:** due componenti con cicli di vita diversi e una futura autorizzazione dei controller renderebbero ogni modifica del registro una riscrittura dell'identità, con un backup condiviso e una coerenza più difficile da esprimere.
- **Una revisione per file di identità:** non esiste una generazione precedente da distinguere e nessun consumatore della revisione; sarebbe un campo non verificabile.
- **Assenza e contenuto iniziale equivalenti:** nasconderebbe la differenza fra un profilo mai inizializzato e un profilo inizializzato, e renderebbe impossibile rifiutare una configurazione presente senza uno dei due file.
- **Scrivere la configurazione durante la prima inizializzazione:** mescolerebbe la decisione di P7 con la persistenza di P4 e creerebbe una configurazione senza passare dallo `Store` che la possiede.
- **Riparare o azzerare contenuti non iniziali:** in D3 non esiste un'identità reale né una migrazione; azzerare un contenuto futuro o sconosciuto distruggerebbe dati senza una regola.
- **Coppia atomica con due temporanei e due rinomine in sequenza:** la seconda rinomina può fallire dopo la prima, quindi l'atomicità non sarebbe reale e la promessa sarebbe falsa. L'ordine con ripresa idempotente è verificabile.
- **Temporanei condivisi con `.config.*`:** la pulizia di un modulo potrebbe rimuovere un temporaneo dell'altro, soprattutto durante un arresto; namespace separati rendono la pulizia locale e sicura.
- **Riusare `config::Store` per scrivere identità:** la configurazione ha revisioni, backup e recupero che i file di identità non hanno; riusarla imporrebbe un formato e una semantica non voluti.
- **Generare chiavi o impronte per «inizializzare davvero»:** fuori perimetro e senza una decisione su algoritmi, formati e protezione.

**Conseguenze.**

- `identity::classify` è in sola lettura e utilizzabile senza lock; `identity::initialize` è l'unico scrittore e richiede `&mut LockedRoot`, quindi l'esclusività è statica e non dipende da un mutex interno.
- La configurazione riceve stati tipizzati e resta l'unica proprietaria di `config.json`, del backup e delle migrazioni; l'identità non li modifica.
- Dopo la prima inizializzazione complete, una riapertura non scrive nulla e `Startup::created` è vuoto.
- Il collegamento all'avvio è stato aggiunto da P8 (AD-010); restano fuori pairing, autorizzazione dei controller, modifica dei grant, revoca, `resetIdentity`, migrazioni dei file di identità e lo schema delle voci future.

**Limiti.** Le prove sono eseguite su macOS; le prove d'integrazione compilano per `x86_64-pc-windows-gnu` ma non sono eseguite su Windows o Linux. I reparse point sono verificati soltanto in compilazione. Le ACL effettive e l'isolamento fra account sono stati osservati soltanto dalla CI (registro Windows e TB-08 Linux); un file posseduto da un altro account richiede privilegi e non è stato provato; un proprietario errato è un rifiuto per costruzione. La durabilità alla perdita di alimentazione non è provata: gli arresti simulati usano `abort` in processi figli, riconosciuto dal padre come `SIGABRT` su Unix e come il codice `fastfail` su Windows, mai come una qualunque uscita fallita. Il lock è consultivo e non protegge da processi che lo ignorano.

**Revisione.** Il modulo è collegato all'avvio da P8 (AD-010). Da rivedere quando esisterà un'identità reale da persistere, quando servirà una migrazione dei file di identità o quando una prova reale su Windows o Linux richiederà regole diverse.

## AD-010 — Avvio operativo del daemon e permanenza senza attesa attiva

**Data:** 27 novembre 2026. **Stato:** accettata per D3 P8. **Ambito:** percorso operativo del binario `totum-daemon`, ordine di avvio, segnalazioni su standard error, permanenza del processo e terminazione.

**Contesto.** P8 collega per la prima volta i moduli `platform`, `identity` e `config` a un binario. Il processo deve preparare, bloccare e classificare la radice prima di qualunque lettura o scrittura dei documenti, completare la sola inizializzazione mancante, aprire lo `Store` e restare in esecuzione senza rete, canali, standard input o attesa attiva. Gli errori devono essere distinti dall'uso scorretto degli argomenti, un commit già avvenuto non deve essere presentato come un guasto, e la terminazione deve affidarsi al sistema operativo invece di installare gestori di segnale. Nessuna nuova funzionalità — listener, autostart, installer, servizio, adapter, pairing, crittografia o comandi diagnostici — è parte del passo. Il processo non deve essere avviato sul profilo reale durante le prove.

**Decisione.**

- **Argomenti prima dello stato.** `totum-daemon` valuta gli argomenti prima di qualunque accesso al file system. `--version` scrive `totum-daemon <versione>` su standard output ed esce con 0; qualunque altro argomento, compresi quelli in più dopo `--version`, produce un messaggio con l'argomento rifiutato e l'uso su standard error, esce con 2 e non tocca lo stato. Senza argomenti parte il percorso operativo.
- **Ordine unico e nessuna regola duplicata.** Il percorso operativo è `platform::state_root` → `platform::prepare_root` → `platform::lock` → `startup::open`. `prepare_root` verifica gli antenati, rifiuta l'esecuzione elevata, crea i soli componenti mancanti e ancora la radice; `lock` acquisisce il lock esclusivo senza attesa; `startup::open` chiama `identity::initialize(&mut LockedRoot)`, riclassifica identità e registro, ne richiede lo stato iniziale e apre `config::Store` con la coerenza aggiornata. Il binario non reimplementa classificatori, persistenza o controlli di sicurezza e distingue assenza, stato iniziale, contenuto non iniziale e stato incoerente tramite gli errori dei moduli.
- **Nessuna scrittura prima della classificazione completa.** Identità, registro, configurazione e backup vengono classificati prima di ogni modifica; solo i file iniziali mancanti vengono completati, prima `identity.json` e poi `controllers.json`, senza riscrivere quelli presenti e senza creare la configurazione. Lo `Store` è aperto soltanto dopo la riclassificazione, così prima esecuzione, caricamento normale, recupero dal backup e migrazione in avanti seguono le decisioni già fissate da P4 e P6, sotto lo stesso lock.
- **Segnalazioni senza confondere commit ed errore.** Prima esecuzione, recupero, migrazione, un backup inutilizzabile accanto a un principale valido e `CommittedNotDurable` sono riportati su standard error prima della riga di prontezza. Un commit effettuato senza durabilità confermata è descritto come operazione riuscita, non ritentata; un rifiuto è `avvio non riuscito: <causa>` con codice 1 e nessuna prontezza. Lo standard output del percorso operativo resta vuoto.
- **Proprietà per tutta la vita del processo.** Dopo la prontezza il binario sposta lo `Store` in una variabile viva fino alla terminazione, quindi la radice ancorata e il lock restano posseduti per tutta la durata del processo; non esistono comandi, richieste o percorsi che li rilascino.
- **Permanenza senza attesa attiva né canali.** Il processo attende con `std::thread::park()` in un ciclo che non termina, senza polling, timeout, socket, pipe, thread aggiuntivi o letture dello standard input. Un risveglio non richiesto riprende l'attesa.
- **Terminazione predefinita del sistema.** Non vengono installati gestori di segnale. Su Unix `SIGTERM`, `SIGINT` e gli altri segnali letali chiudono il processo con la semantica predefinita; su Windows la chiusura della console o del processo ha lo stesso effetto. In entrambi i casi il sistema operativo rilascia il lock e `state.lock` non viene cancellato né riscritto: un nuovo avvio riapre lo stesso file.
- **Prove con processi figli e radici temporanee.** Le prove d'integrazione avviano il binario reale con le variabili di profilo (`HOME` su macOS, `HOME` e `XDG_STATE_HOME` su Linux, `LOCALAPPDATA` su Windows) puntate a directory temporanee sotto `target/`, attendono la prontezza con un limite di tempo, raccolgono l'esito e terminano il figlio, con pulizia garantita. Le prove di P8 verificano prima esecuzione e riavvio senza riscritture, stati iniziali parziali, rifiuti, recupero, migrazione, esclusione della seconda istanza e riacquisizione del lock, assenza di stato per `--version` e per l'uso scorretto, prontezza solo dopo il successo e nessun socket di rete aperto dal processo residente. I test D1 che prevedevano l'uscita immediata sono stati rimossi, mantenendo coperti versione e uso scorretto.
- **`totum status` invariato.** La CLI `totum` conserva il comportamento D1: dichiara che il collegamento al daemon non è implementato e non esegue controlli.

**Alternative scartate.**

- **Valutare gli argomenti dopo l'apertura dello stato:** un `--version` o un errore d'uso creerebbe o modificherebbe file, contro il requisito che gli argomenti siano valutati prima di qualunque accesso.
- **Duplicare nel binario classificazione, recupero o migrazione:** due implementazioni delle stesse regole divergerebbero e renderebbero ambigua la provenienza di un commit.
- **Completare i file di identità dopo l'apertura dello `Store`:** la configurazione riceverebbe una coerenza non aggiornata e la prima esecuzione potrebbe essere rifiutata per uno stato che il passo stesso può completare.
- **Uscire dopo la prontezza o restare con un ciclo di attesa attiva:** un'uscita immediata non è un daemon; un ciclo con `sleep`, polling o timeout consuma risorse e introduce una semantica di arresto non richiesta.
- **Leggere lo standard input per restare vivi:** legherebbe la vita del processo a un canale che il perimetro esclude e impedirebbe l'avvio da un servizio senza terminale.
- **Installare gestori di segnale o un percorso di arresto ordinato:** aggiungerebbe una responsabilità di arresto, una stampa di chiusura e una garanzia sul rilascio che il sistema già fornisce alla morte del processo.
- **Aprire un socket locale per dimostrare che il processo è vivo:** introdurrebbe rete e un canale fuori perimetro; la liveness si verifica dal file system e dai descrittori del processo.
- **Un nuovo comando `totum-daemon run` o opzioni di avvio:** non aggiunge nulla al comportamento richiesto e simulerebbe funzionalità future.
- **Una dipendenza per il daemon o per l'attesa:** la libreria standard copre argomenti, thread e attesa; nessuna dipendenza nuova è ammessa.

**Conseguenze.**

- L'avvio è deterministico: gli stessi controlli e lo stesso ordine di P1–P7, un solo punto di prontezza e nessuna scrittura dopo un rifiuto.
- Il processo residente trattiene radice e lock: una seconda istanza fallisce con `AlreadyRunning` finché la prima è viva, e la riacquisizione riesce dopo la terminazione.
- Un commit riuscito con durabilità non confermata resta un successo segnalato; un errore prima del commit non produce prontezza.
- Nessuna nuova dipendenza e nessuna modifica a `Cargo.lock`; il binario usa la libreria standard, `platform`, `identity`, `config` e `startup`.
- Restano fuori: autostart, installer, servizio di sistema, hub operativo, listener, adapter, pairing, crittografia, protocollo mobile o relay e qualunque comando che simuli funzionalità future.

**Limiti.** Le prove sono eseguite su macOS. Il percorso operativo non è stato eseguito su Windows o Linux: la compilazione per `x86_64-pc-windows-gnu` è verificata, l'esecuzione no. La terminazione è provata con `SIGTERM` su macOS; la chiusura della console Windows non è stata eseguita. L'assenza di socket è verificata con `lsof` quando disponibile, non come garanzia del sistema. Le ACL effettive e l'isolamento fra account sono stati osservati soltanto dalla CI; un `state.lock` posseduto da un altro account non è stato provato. La durabilità alla perdita di alimentazione non è provata. La CI del repository è stata eseguita, ma non è ancora verde.

**Revisione.** Quando si aggiungerà un arresto ordinato, un servizio di sistema o un listener, o quando una prova reale su Windows o Linux richiederà una semantica di terminazione diversa.

## AD-011 — Avvio automatico per account come primitive di libreria

**Data:** 27 novembre 2026. **Stato:** accettata per D3 P9. **Ambito:** primitive di stato, registrazione e rimozione dell'avvio automatico per account, senza attivazione e senza comandi.

**Contesto.** P9 deve rendere registrabile l'avvio del daemon al prossimo accesso dell'utente con tre meccanismi per account, senza avviare il processo, senza caricare servizi, senza scrivere configurazioni di sistema e senza dipendere da strumenti esterni. Gli oggetti convivono con file che l'utente o altri programmi possono aver creato: la registrazione deve essere idempotente, lo stato in sola lettura, la rimozione limitata agli oggetti riconosciuti come propri e ogni collisione deve essere un rifiuto esplicito. La presenza di una registrazione non dimostra che il daemon sia attivo o che partirà: la prova di un accesso reale e l'attivazione appartengono a P10. Le prove non devono toccare il profilo reale.

**Decisione.**

- **Un meccanismo per piattaforma, tutti per account.** macOS usa un Launch Agent in `$HOME/Library/LaunchAgents/app.homun.totum.daemon.plist` con `Label` `app.homun.totum.daemon`, `ProgramArguments` di un solo elemento e `RunAtLoad` vero, senza `KeepAlive`, senza `StandardOutPath`, `StandardErrorPath`, `WorkingDirectory` o altri campi. Linux usa l'unità utente `$XDG_CONFIG_HOME/systemd/user/totum-daemon.service` (oppure `$HOME/.config/systemd/user/...`) con `WantedBy=default.target` e un collegamento relativo `default.target.wants/totum-daemon.service` → `../totum-daemon.service`. Windows usa il valore `app.homun.totum.daemon` di tipo `REG_SZ` in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. Il nome `app.homun.totum.daemon` è provvisorio come il resto dei contratti di lavoro.
- **Nessuna attivazione e nessuno strumento di sistema.** Il modulo non esegue `launchctl`, `systemctl`, `systemd-analyze`, `reg.exe` o processi esterni, non chiama `launchctl load`, non abilita l'unità, non esegue il daemon, non avvia la sessione utente e non modifica linger, logout o impostazioni di sistema. La registrazione è soltanto la scrittura dell'oggetto previsto; il caricamento al prossimo accesso è responsabilità del sistema e non è verificato qui. Su Linux la disponibilità di systemd è stimata con l'esistenza di `/run/systemd/system`, la stessa condizione di `sd_booted()`, registrata nel `Layout` al momento della costruzione.
- **Stato in sola lettura con quattro esiti.** `status()` restituisce `Registered { program }` quando l'oggetto è presente, riconosciuto come proprio e con un programma valido; `Absent` quando l'oggetto o la directory non esistono; `Incompatible { detail }` quando esiste un oggetto con nome previsto ma contenuto, tipo, proprietario, modalità o collegamento diversi da quelli attesi; `Unavailable { detail }` quando il meccanismo non è disponibile (Linux senza systemd, o `RunKey` su una piattaforma non Windows). `Registered` non significa attivo né caricato: descrive soltanto l'oggetto su disco o nel registro. Lo stato non crea directory, non ripara protezioni e non segue collegamenti arbitrari.
- **Formati minimi, riconoscimento rigoroso.** Il plist e l'unità sono generati da modelli fissi con il solo percorso del programma interpolato; la lettura accetta soltanto il documento identico alla codifica canonica del programma riconosciuto. Un plist con `Label` diversa, `RunAtLoad` falso, `KeepAlive`, argomenti aggiuntivi, campi extra o una resa non canonica del testo non è proprio. Un'unità con direttive aggiuntive, `ExecStart` diverso o un percorso non riconosciuto non è propria. Sul registro un valore `REG_SZ` è letto come testo letterale e rianalizzato con la stessa codifica; un tipo diverso, compreso `REG_EXPAND_SZ`, è incompatibile. Protezioni non ammesse della chiave `Run` o di un suo antenato rendono lo stato `Incompatible` senza modifiche; una chiave assente resta `Absent`. Non esiste una modalità «quasi proprio»: o il documento coincide con quello che il modulo scriverebbe per quel programma, o è estraneo.
- **Percorso assoluto, nessun argomento, nessun segreto.** Il programma è un percorso assoluto, non la radice, senza separatore finale, senza componenti `.` o `..`, in UTF-8 e senza caratteri di controllo. Non è ammesso alcun argomento, variabile, comando shell, segreto o token. Un percorso non rappresentabile in sicurezza per il formato scelto produce `NotRepresentable` prima di qualunque scrittura.
- **Codifica per formato.** Nel plist il percorso è il solo testo dell'elemento `ProgramArguments` e viene codificato con le entità XML per `&`, `<`, `>`, `"` e `'`. Nell'unità systemd `ExecStart` non passa da una shell: il primo argomento è racchiuso fra virgolette doppie, preceduto dal prefisso `:` documentato da `systemd.service(5)`, con escape di `\` e `"`. Il prefisso imposta `EXEC_COMMAND_NO_ENV_EXPAND` in `config_parse_exec` (`load-fragment.c`), e il percorso ufficiale di esecuzione salta `replace_env_argv` quando il flag è presente (`exec-invoke.c`): `$HOME`, `${HOME}` e `$$` restano letterali e non vengono espansi. I specificatori `%` vengono invece risolti lo stesso (`unit_path_printf`), quindi una percentuale letterale resta codificata come `%%`. Il parser interno riconosce soltanto la forma con il prefisso `:` e rifiuta quella senza. Nel registro il valore è `REG_SZ`, che Windows non espande: `%` e `$` restano letterali e non viene usato `REG_EXPAND_SZ`; il percorso è racchiuso fra virgolette solo se contiene spazi o tab, con la codifica degli argomenti di MS C, e la riga completa è limitata a 260 unità UTF-16. Un percorso con virgolette doppie non è rappresentabile nel registro e viene rifiutato invece di essere approssimato.
- **Registrazione idempotente con risultato esplicito.** `install(&Path)` classifica prima di scrivere e restituisce `InstallReport { program, entry, link, durability }`: `Created`, `Updated`, `Unchanged` o `NotAttempted` per l'oggetto principale, lo stesso per il collegamento systemd, e `Durability::{Unknown, Confirmed, NotConfirmed}`. Un secondo `install` con lo stesso programma non riscrive l'oggetto e riporta `Unchanged` con durabilità `Unknown`; un oggetto proprio con un programma diverso viene sostituito con `Updated`; un oggetto estraneo o incompatibile non viene toccato e produce `IncompatibleObject`.
- **Nessuna sovrascrittura e controllo fra classificazione e scrittura.** Le scritture riusano le primitive ancorate di `platform`: temporaneo esclusivo 0600, sincronizzazione del file, pubblicazione che non sostituisce per un oggetto assente e sostituzione atomica per un oggetto riconosciuto come proprio. Prima della sostituzione l'oggetto viene riclassificato e il contenuto precedente confrontato con quello appena letto: se cambia, l'operazione si ferma con `ChangedDuringRegistration` e non sovrascrive il lavoro altrui. Un oggetto comparso fra assenza e pubblicazione produce lo stesso rifiuto.
- **Collegamento systemd riconosciuto senza seguirlo.** Il collegamento in `default.target.wants` è proprio se è un collegamento simbolico posseduto dall'utente corrente e il suo testo è esattamente `../totum-daemon.service` oppure il percorso assoluto dell'unità. Il testo viene letto con `readlinkat` e non viene mai seguito per leggere o modificare la destinazione. Non esistendo una sostituzione atomica di collegamento nelle primitive disponibili, un collegamento proprio con testo diverso non viene sostituito: è `IncompatibleObject`. Un collegamento inatteso o posseduto da altri è un rifiuto.
- **Directory: creare solo ciò che manca, verificare il resto.** Le directory mancanti del percorso (per esempio `~/Library/LaunchAgents`, `systemd/user`, `default.target.wants`) vengono create con modalità 0700 e verifica delle ACL; le directory già presenti non vengono mai modificate, ma devono appartenere all'utente corrente e non essere scrivibili da gruppo o altri. Una directory con protezioni errate produce `UnsafeObject` e resta invariata. Su Windows il registro non richiede elevazione, ma l'appartenenza a `HKCU` non sostituisce la verifica delle protezioni descritta sotto; un processo elevato viene rifiutato come già avviene per la radice di stato.
- **Protezioni della chiave `Run` verificate sugli handle effettivi.** L'appartenenza a `HKCU` e il successo di `RegOpenKeyExW` non dimostrano che altri account standard non possano modificare la chiave. Prima di ogni scrittura o eliminazione il backend legge proprietario e DACL effettivi dell'handle scrivibile della chiave `Run` e degli antenati `Software`, `Software\Microsoft`, `Software\Microsoft\Windows` e `Software\Microsoft\Windows\CurrentVersion`, e li ricontrolla subito prima della modifica. Una DACL assente, un proprietario diverso dall'account corrente, SYSTEM o `BUILTIN\Administrators`, una voce consentita a un altro titolare con diritti di modifica o sostituzione, o una voce di tipo non interpretabile sono un rifiuto `UnsafeObject`; le voci inherit-only, Denied, Audit e Alarm non concedono diritti. La decisione è pura in `autostart::registry` e provata con dati sintetici e un backend di prova. Lo stato applica la stessa politica in sola lettura: una registrazione presente sotto un antenato non sicuro è `Incompatible`, con la diagnosi dell'antenato, mentre una chiave assente resta `Absent` senza valutare protezioni di oggetti inesistenti. La creazione usa un descrittore di sicurezza esplicito (proprietario corrente, DACL protetta con accesso completo al solo account corrente, SYSTEM e amministratori) e richiede `REG_CREATED_NEW_KEY`: una chiave comparsa o scomparsa fra classificazione e scrittura produce `ChangedDuringRegistration`. La chiave condivisa non viene riparata e gli altri valori non vengono toccati; il valore viene riconfrontato con lo snapshot prima di essere scritto o eliminato.
- **Rimozione esplicita e simmetrica.** `remove()` elimina soltanto oggetti riconosciuti come propri, prima il collegamento e poi l'unità, oppure il file o il valore; un secondo `remove()` riporta `AlreadyAbsent` e non è un errore. Un oggetto estraneo o incompatibile non viene mai cancellato. La directory condivisa `default.target.wants` non viene rimossa; le directory create dal modulo non vengono rimosse da `remove()`, che si limita agli oggetti di Totum.
- **Risultati parziali senza falsi successi.** Su systemd l'unità viene scritta prima del collegamento. Se il collegamento non può essere creato, `install` restituisce `InstallFailure` con `entry: Created` o `Updated`, `link: NotAttempted` e l'errore; lo stato successivo è `Incompatible` perché l'unità esiste senza collegamento, e un `install` ripetuto completa il solo collegamento. Un commit con sincronizzazione della directory non riuscita è riportato come `NotConfirmed` senza annullare l'oggetto.
- **Nessuna dipendenza nuova oltre una feature.** Il modulo usa la libreria standard, `platform`, `serde`/`serde_json` solo dove già presenti e `windows-sys` per il registro; la feature `Win32_System_Registry` viene aggiunta all'elenco già ammesso. Non vengono introdotti comandi CLI, collegamenti all'avvio del binario, installer, servizi o modifiche alla CI.
- **Prove isolate.** Le prove usano `Layout` espliciti con directory temporanee sotto la directory temporanea di sistema e non scrivono mai `~/Library/LaunchAgents`, `~/.config/systemd/user` o il registro del profilo reale. La disponibilità di systemd è iniettata per provare anche il caso non disponibile su macOS. Il backend del registro Windows è compilato per `x86_64-pc-windows-gnu` ma non eseguito; nessun login reale è stato verificato.

**Alternative scartate.**

- **Invocare `launchctl` o `systemctl` per caricare e abilitare:** attiverebbe la sessione corrente e renderebbe la registrazione dipendente da strumenti e da uno stato esterno, contro il requisito di non attivare nulla.
- **Un servizio di sistema o un'installazione con privilegi:** fuori perimetro, richiederebbe elevazione e cambierebbe il modello per account.
- **`KeepAlive` o riavvio automatico nel plist:** il piano richiede `RunAtLoad` senza `KeepAlive`; un riavvio automatico introdurrebbe una politica di supervisione non decisa.
- **`REG_EXPAND_SZ` per il valore di avvio:** espanderebbe `%` e `$` e cambierebbe il significato di un percorso legittimo; il valore deve restare il percorso letterale.
- **Considerare sufficiente l'appartenenza a `HKCU` o il successo di `RegOpenKeyExW`:** una DACL permissiva o un proprietario diverso possono permettere a un altro account di sostituire il valore; la verifica deve riguardare le protezioni effettive e i diritti che consentono di modificare o sostituire la chiave.
- **Riparare la DACL della chiave `Run`:** è una chiave condivisa con altri valori e una riscrittura cambierebbe oggetti non creati dal modulo, nascondendo una manomissione; una protezione non conforme è un rifiuto.
- **Raddoppiare `$` in `ExecStart`:** senza il prefisso `:` un dollaro letterale si scrive `$$`, ma con `EXEC_COMMAND_NO_ENV_EXPAND` la sostituzione non avviene e `$$` arriverebbe al processo come due dollari, cambiando il percorso richiesto. Il prefisso documentato e il testo letterale sono la forma corretta.
- **Scrivere un comando con argomenti o una riga di shell:** il daemon non ha argomenti in P9 e una shell introdurrebbe quoting, injection e dipendenze non volute.
- **Seguire il collegamento `default.target.wants` per verificare l'unità:** permetterebbe a un collegamento estraneo di far leggere o modificare un percorso arbitrario; il testo del collegamento è sufficiente a riconoscere quello previsto.
- **Sostituire un collegamento proprio con testo diverso:** non esiste una primitiva atomica di sostituzione del collegamento e una sequenza rimuovi-crea lascerebbe una finestra senza collegamento; il caso è un rifiuto esplicito.
- **Riparare modalità o proprietario di oggetti e directory esistenti:** nasconderebbe una manomissione e cambierebbe oggetti non creati dal modulo.
- **Rimuovere le directory create dal modulo:** una directory condivisa può contenere altri oggetti e la rimozione non è necessaria per annullare la registrazione.
- **Riconoscere come proprio qualunque documento con il `Label` o il nome del valore:** il contenuto potrebbe essere stato modificato da altri; il confronto canonico evita di sovrascrivere configurazioni estranee.
- **Dichiarare `Registered` come «daemon attivo»:** la presenza di un oggetto non dimostra che il sistema lo abbia caricato o che il daemon sia in esecuzione.

**Conseguenze.**

- `autostart::{install, status, remove}` sono API di libreria pure rispetto al processo: non avviano il daemon e non sono collegate all'avvio del binario né a un comando CLI.
- La registrazione è ripetibile senza duplicati e la rimozione è ripetibile senza errori; ogni collisione è un rifiuto che lascia intatto l'oggetto esistente.
- Su Windows la chiave `Run` viene creata soltanto con protezioni imposte e verificate, e una chiave preesistente non conforme è un rifiuto; su systemd l'unità conserva il percorso letterale e il prefisso `:` è parte della forma riconosciuta.
- I risultati parziali di systemd sono espliciti e una registrazione interrotta può essere completata da una chiamata successiva.
- Restano fuori: attivazione della sessione, caricamento, verifica di un login reale, installer, servizio di sistema, comandi CLI, rete, adapter, pairing e qualunque modifica alla CI.
- Il perimetro delle dipendenze cambia solo per la feature `Win32_System_Registry` di `windows-sys`; il test del perimetro continua a passare senza nuove crate.

**Limiti.** Le prove sono eseguite su macOS con directory temporanee. Il backend del registro Windows è stato eseguito nella CI del 27 novembre 2026 (`registry-windows`, run 46010768568): le prove hanno letto le DACL, creato una chiave con descrittore protetto e scritto in `HKCU` di una sottochiave isolata. La politica delle protezioni resta provata anche con dati sintetici e un backend di prova. Il backend systemd è lo stesso codice Unix usato per il Launch Agent ed è provato con disponibilità iniettata, e `systemd-analyze` ha verificato il documento nella CI Linux; non è stato eseguito oltre l'analisi, quindi non sono verificati `sd_booted()`, il caricamento dell'unità né l'avvio al login. La semantica del prefisso `:` e il salto di `replace_env_argv` sono verificati sul sorgente ufficiale di systemd (`config_parse_exec` e `exec-invoke.c`), non con un'esecuzione del servizio su Linux. Nessun accesso reale è stato provato: appartiene a P10. Le prove non scrivono mai il profilo reale e non dichiarano che il daemon parta al login. Le ACL effettive e l'isolamento fra account sono stati osservati soltanto dalla CI (registro Windows e TB-08 Linux); l'isolamento su macOS e Windows non è provato. La CI del repository è stata eseguita, ma non è ancora verde: la matrice Rust è fallita per tre cause corrette ma non rieseguite e il job `windows-non-elevated` si è fermato prima delle prove.

**Revisione.** Da rivedere quando P10 collegherà la registrazione all'avvio del binario o a un comando, quando si vorrà attivare o verificare il caricamento, quando cambieranno i formati di launchd o systemd, o quando una prova reale su Windows o Linux richiederà una regola diversa. Il nome `app.homun.totum.daemon` è provvisorio e va rivisto con l'identificativo definitivo del prodotto.

## AD-012 — Verifiche eseguibili di P1–P9 e attrezzatura di prova (P10-A)

**Data:** 27 novembre 2026. **Stato:** accettata per P10-A. **Ambito:** test, harness e job di verifica della CI. Modifica il limite di AD-011 «nessuna modifica alla CI» soltanto per i job di verifica introdotti qui.

**Contesto.** Il passo P10 richiede prove che P1–P9 non potevano eseguire: TB-08 con due account sulla CI Linux, il backend reale del registro su Windows e il formato systemd confrontato con il consumatore reale. L'autorizzazione di P10-A vieta funzionalità applicative nuove, account sul computer locale, modifiche alla chiave `Run` reale e attivazione di servizi. Serviva quindi una strategia di prova che non cambiasse il codice di produzione e che non dichiarasse eseguibile ciò che l'ambiente non consente. La revisione di questo passo ha inoltre imposto che l'harness non possa eliminare account preesistenti, che i verdetti non accettino un errore qualunque come un diniego e che la prova Windows misuri una creazione reale e non una rigenerazione.

**Decisione.**

- **Registro Windows: sottochiave isolata, percorso non iniettabile in produzione.** Le prove di libreria preparano `HKCU\Software\Homun\Totum\P10ATests\<pid>-<contatore>` creando **soltanto gli antenati** e pretendendo che la chiave manchi: è il backend a crearla, quindi `InstallChange::Created` corrisponde a `REG_CREATED_NEW_KEY` e la prova controlla la chiave realmente prodotta (proprietario, DACL presente, bandiera `SE_DACL_PROTECTED` esposta come `RegistrySecurity::protected`, soli titolari di policy, nessuna voce ereditata). Esercita poi stato, idempotenza, rifiuto di un valore estraneo e rimozione, e rimuove la sottochiave al termine. La prova del rifiuto è un caso separato che parte da una chiave già creata con la DACL di policy, così il backend la incontra preesistente: concede a `BUILTIN\Users` `KEY_SET_VALUE` sulla sola chiave di prova, confronta le protezioni lette prima e dopo l'alterazione e, se l'alterazione non è applicabile, dichiara l'ambiente su standard error e salta i verdetti invece di ripristinare ciò che non ha cambiato; se il ripristino fallisce, la sottochiave con l'ACE estraneo viene rimossa dall'impl Drop della prova. Il backend di produzione resta vincolato a `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` e non ha opzioni, variabili o bypass per cambiare percorso; il valore `Run` reale viene soltanto confrontato prima e dopo, mai scritto. La sottochiave è stata preferita a `HKLM` perché il token di prova non ha `WRITE_DAC` sulle chiavi di sistema e la prova del rifiuto non sarebbe eseguibile senza elevazione.
- **systemd: consumatore reale, sola analisi.** Due prove d'integrazione installano l'unità in una directory temporanea e la verificano con `systemd-analyze --man=no verify` quando lo strumento esiste: la prima con un percorso contenente spazi, `$HOME` e `100%` il cui eseguibile esiste, la seconda con l'esecuzione mancante, che deve essere rifiutata perché un'unità che punta a un binario inesistente non verrebbe mai usata. Le due prove condividono la stessa codifica, prodotta esclusivamente dal modulo. Non viene caricato, abilitato o avviato alcun servizio e il modulo applicativo continua a non invocare strumenti di sistema. Su macOS la prova si salta con un messaggio esplicito; in CI `TOTUM_REQUIRE_SYSTEMD_ANALYZE=1` rende l'assenza dello strumento un fallimento, così la verifica non può passare per omissione.
- **TB-08: solo ambiente dichiarata, account registrati.** L'harness `.github/scripts/tb08-linux.sh` applica i controlli d'ambiente prima di qualunque modifica, in quest'ordine: Linux, `TOTUM_TB08_CI=1`, `GITHUB_ACTIONS=true` o `CI=true`, `RUNNER_ENVIRONMENT=github-hosted`, poi root — il solo root non basta, e il job li trasmette attraverso `sudo env`. I nomi derivano dal commit; un account già esistente ferma la prova e **non viene mai terminato né rimosso**: solo gli account effettivamente creati, registrati come `nome:uid`, sono puliti, in modo idempotente e anche per `INT`/`TERM`/`HUP`. Home e antenati sono attraversabili per isolare la causa del rifiuto, lo stato vive nella home del primo account, e il secondo account tenta dodici operazioni fra lettura, elenco, scrittura, creazione, collegamento, rinomina, sostituzione, scambio di directory e modifica delle protezioni. Un diniego è accettato solo con il codice che lo strumento usa per un accesso negato — GNU `ls` esce 2 quando non riesce ad aprire una directory o un argomento, mentre `cat`, `touch`, `ln`, `mv`, `chmod` e la redirezione di bash escono 1 e usano 2 per l'uso scorretto — rifiutando timeout (124/125), mancata esecuzione (126/127) e segnali (≥128), **e** con il testo del kernel (`Permission denied`/`Operation not permitted`), preceduto da un controllo positivo sugli stessi oggetti di proprietà dell'esecutore: un errore dell'harness non è un diniego. Il secondo daemon è accettato solo con uscita 1, «avvio non riuscito», diagnosi di proprietario o protezioni, radice estranea nominata e nessuna prontezza. Il manifesto è confrontato con differenza unificata e una tabella a schermo elenca esito e motivazione di ogni caso. Un `--self-test` non privilegiato, invocato anche da `cargo test` con `tests/tb08_harness.rs`, ripete i verdetti con comandi simulati, comprese le regressioni per singolo strumento (GNU `ls` con uscita 2 e diagnosi del kernel valida; argomenti errati, percorso inesistente, timeout e crash non conclusivi) e prove di onestà (bersaglio leggibile e percorso inesistente), senza toccare account reali. `INT`, `TERM` e `HUP` sono provati in sottoprocessi dedicati con gli stessi comandi simulati: terminano con 130, 143 e 129, eseguono una sola pulizia e non lasciano proseguire alcuna operazione privilegiata, sia prima di qualunque creazione sia dopo una creazione parziale, dove viene rimosso solo l'account registrato.
- **CI: job aggiunti, matrice Windows ristretta alle prove senza stato.** `registry-windows` esegue le prove del registro, `systemd-linux` le due prove con `systemd-analyze`, `tb08-selftest` il self-test non privilegiato su macOS e Ubuntu, `tb08-linux` l'harness con due account effimeri tramite `sudo env` che propaga il contesto, `windows-non-elevated` le prove di stato con un account non elevato. Il job di spazi resta invariato; formato, `clippy` e build restano su tutti i sistemi. Su Windows però la matrice non esegue più le prove di stato, perché il runner è amministratore con UAC disabilitato e `platform::prepare_root` rifiuta correttamente un token elevato: esegue soltanto contratti, CLI e punti d'ingresso del daemon.
- **Windows non elevato: account effimero, token verificato, fasi separate.** Il job `windows-non-elevated` crea un account locale effimero non amministratore soltanto quando i controlli dichiarano un runner GitHub-hosted (`GITHUB_ACTIONS=true`, `RUNNER_ENVIRONMENT=github-hosted`, `GITHUB_RUN_ID` presente), con timeout e pulizia che rimuove account, profilo e albero di lavoro anche in caso di errore. Compilazione ed esecuzione restano fasi distinte: con l'account del runner `cargo test --no-run` compila la libreria e i cinque target di stato, `cargo build --bin totum-daemon` produce il binario reale e tutto avviene in un albero copiato su volume di sistema e protetto con `icacls`. Dopo la prova di rifiuto dell'elevazione, che deve precedere il cambio di proprietario perché la camminata degli antenati del runner vedrebbe altrimenti un proprietario estraneo, l'albero viene ceduto all'account di prova con `icacls /setowner`: `platform::prepare_root` esige che ogni antenato esistente appartenga all'utente corrente o a un principale di sistema, e le directory di prova sotto `target/` sono state create dal runner. I binari già compilati girano poi con l'account di prova tramite `Start-Process -Credential -LoadUserProfile`. Prima delle prove di stato uno script separato legge `GetTokenInformation(TOKEN_ELEVATION)` e confronta il SID dell'account con quello atteso: un token elevato, un'identità diversa o il ruolo di amministratore fermano il job, perché il nome del processo e la modalità di lancio non dimostrano nulla. Le prove di stato (`config`, `identity`, `lock`, `platform_windows`, `startup`, `totum_daemon`) girano con `--skip real_backend`, così le prove del registro restano nel solo job `registry-windows` sulla sottochiave isolata. La verifica che il daemon rifiuti l'esecuzione elevata resta separata e avviene prima, con il token elevato del runner: uscita 1, diagnosi esplicita e nessuna directory di stato creata.

**Alternative scartate.**

- **Provare la registrazione sul valore `Run` reale e poi ripristinarlo:** il ripristino non è garantito in caso di interruzione e un valore reale non è un oggetto di prova; la verifica del solo confronto lascia la chiave intatta.
- **Provare in `HKLM`:** richiede privilegi e non permette di costruire il caso di DACL modificabile da un altro gruppo senza elevazione.
- **Aggiungere al backend di produzione un percorso configurabile per le prove:** sarebbe un bypass disponibile anche in produzione; la sottochiave isolata si costruisce con le primitive esistenti e resta interna al test.
- **Invocare `systemctl --user start` o `enable` per verificare l'unità:** attiverebbe la sessione e violerebbe il divieto di attivazione; `systemd-analyze verify` analizza il documento senza caricarlo.
- **Saltare la prova systemd quando lo strumento manca, anche in CI:** renderebbe la verifica invisibile; la variabile di CI distingue il caso locale dal requisito.
- **Creare account effimeri su macOS o Windows per una prova locale:** vietato dall'autorizzazione e non necessario; l'account Windows esiste soltanto nella predisposizione del job su un runner GitHub-hosted isolato, e le prove manuali restano documentate come non eseguite.
- **Eseguire le prove di stato come amministratore e disattivare il controllo di elevazione nei test:** avrebbe nascosto il comportamento reale del prodotto e introdotto un bypass; la matrice esegue soltanto le prove che non toccano lo stato e il job dedicato usa un token non elevato verificato.
- **Presumere non elevato un processo lanciato in modo diverso:** `Start-Process`, un profilo ridotto o un nome diverso non cambiano il token; la prova legge `TOKEN_ELEVATION` e confronta il SID prima di eseguire.
- **Creare e rimuovere due nomi fissi nell'harness:** una collisione con un account preesistente avrebbe terminato processi e cancellato un account non creato dalla prova; il registro degli account realmente creati, con il loro uid, rende la pulizia limitata a questa esecuzione e ripetibile.
- **Accettare qualunque uscita non nulla come diniego o come rifiuto del daemon:** avrebbe contato come prove gli errori dell'harness (uso scorretto, comando mancante, crash, timeout); i verdetti richiedono invece un'uscita ammessa, il testo del kernel e, per il daemon, il codice operativo con la diagnosi pertinente.
- **Creare anche la chiave di prova prima di invocare il backend:** la prova avrebbe misurato una rigenerazione e non avrebbe potuto affermare `REG_CREATED_NEW_KEY` né le protezioni di una chiave nata dal descrittore di creazione.
- **Verificare solo che il nostro encoder si inverta:** non dimostrerebbe che il consumatore reale accetti il documento; la prova usa lo strumento ufficiale e confronta anche la forma canonica scritta a mano.

**Conseguenze.**

- Nessuna modifica al comportamento del binario, alle decisioni del modulo `autostart` o ai formati. Il codice condiviso cambia in un solo punto: `platform::RegistrySecurity` espone anche `protected`, valore già letto dal descrittore e prima scartato, usato dalle prove e da nessuna regola di sicurezza.
- Nessuna dipendenza nuova: le prove usano la libreria standard, `windows-sys` già ammesso e strumenti di sistema esclusivamente nell'harness.
- La seconda CI sul commit `70e064c` ([run 46020171656](https://github.com/homun-app/totum-daemon/actions/runs/46020171656)) ha superato 8 job su 10: la matrice Rust Windows e Linux è verde; macOS fallisce per un conflitto intermittente di lock nei test di migrazione; Windows non elevato verifica account, SID e token, ma va in timeout nella suite `config` e fallisce la rimozione dell’albero. La causa del timeout Windows resta da accertare. Le correzioni successive sono verificate soltanto in locale; dettagli e limiti in [ci-verification.md](ci-verification.md).
- L'isolamento fra account su macOS e Windows e il login reale sui tre sistemi restano procedure manuali e non sono dichiarati verificati.

**Limiti.** Le prove Windows dipendono dalle protezioni effettive di `HKCU\Software` sul runner e dalla disponibilità di `WRITE_DAC` sulla sola sottochiave di prova; se l'ambiente negasse l'alterazione, la prova dichiara l'ambiente e salta i verdetti; nella CI del 27 novembre 2026 l'alterazione era applicabile e il rifiuto basato sulla DACL è stato osservato dalla prova superata. TB-08 richiede `useradd`, `setpriv`, `runuser` e un filesystem che rispetti i bit Unix; non copre canali, login, ACL estese né processi dello stesso account, e i dinieghi derivati dalla proprietà non sono simulabili senza privilegi: il self-test copre la macchina dei verdetti, mentre l'isolamento reale è stato osservato dall'harness privilegiato nella CI Linux. Il target Linux non è installato nella toolchain locale, quindi i blocchi specifici di Linux sono compilati soltanto dalla CI. La prova che nega l'accesso a un secondo daemon su una radice altrui è, in locale, una prova di protezioni dello stesso account: il vero `WrongOwner` fra account è stato osservato dalla CI Linux. `systemd-analyze verify` controlla il documento e il percorso eseguibile, non il caricamento né l'avvio reale. Il job `windows-non-elevated` ha creato un account reale nel runner e osservato SID, token non elevato e scrittura nell’albero; la suite `config` è andata in timeout e la pulizia dell’albero è fallita, quindi né le suite di stato né la pulizia completa sono dichiarate verificate. La camminata degli antenati legge gli ACE senza risolvere l'ereditarietà: un profilo o una radice di fallback su Azure Files con un ACE `(M)` non ereditato per `Authenticated Users` farebbe fallire la preparazione con `AclViolation`; il job usa un albero su volume di sistema, ma il caso non è stato provato. La durabilità alla perdita di alimentazione resta non provata.

**Revisione.** Da rivedere quando la CI sarà completamente verde, quando una macchina Windows o macOS dedicata permetterà le prove manuali, o quando l'attivazione dell'avvio automatico e il login reale entreranno nel perimetro.

### Correzione dell’harness dopo la seconda CI

La coordinazione fra spawn e transizioni dei lock è condivisa fra i test di configurazione e identità nel solo modulo `cfg(test)` `test_support`. Si corregge il perimetro del mutex senza introdurre retry né modifiche al lock operativo. L’attesa dei processi Windows conserva i log anche al timeout e stampa avanzamento e metadati dei soli processi interessati; prima di cancellare l’account effimero, la sorgente viene rimossa con il suo token per rispettare le DACL del proprietario. Il self-test non privilegiato prova cattura, uscita e pulizia sintetica; cambio di account e ACL reali richiedono il prossimo run Windows. La causa del timeout resta da accertare. [Evidenze e limiti](ci-verification.md).

## AD-013 — Sostituzione Windows con lettori aperti

**Data:** 27 novembre 2026. **Stato:** accettata come correzione di D3 P4, autorizzata dall’utente. **Ambito:** `platform::replace_file` e prove di stato Windows.

**Evidenza.** La CI del commit `97b5d9d` ([run 46031117777](https://github.com/homun-app/totum-daemon/actions/runs/46031117777)) ha riprodotto errore 5 con un lettore tenuto aperto, anche se aperto con condivisione di eliminazione. È fallita anche la prova concorrente; grazie allo stop durante unwinding, la suite è terminata senza timeout. Le nuove regressioni di snapshot e stop sono passate su Windows.

**Decisione.** Usare `std::fs::rename` della toolchain già fissata a Rust 1.95.0, senza nuove dipendenze o feature. L’implementazione Windows prova `MoveFileExW` e, su accesso negato, apre la sorgente senza seguire reparse point e usa `SetFileInformationByHandle(FileRenameInfoEx)` con `REPLACE_IF_EXISTS | POSIX_SEMANTICS`. Non imposta il flag che ignora il solo attributo di lettura. Il cambio di API è nella libreria standard: non viene introdotto un ciclo di retry né una cancellazione preventiva. Validazione dei nomi, verifica della radice, mappatura degli errori, DACL e pubblicazione senza sostituzione restano invariate.

**Prove.** La regressione Windows mantiene un vecchio handle aperto durante la sostituzione: deve leggere la generazione precedente, mentre il nome pubblico deve aprire quella nuova e il temporaneo deve sparire. Si conserva anche la prova concorrente dei venti salvataggi. Le prove d’identità ora richiedono esplicitamente `CommittedNotDurable` su Windows e commit durevole su Unix, coerentemente con la sincronizzazione disponibile. Se Windows rifiuta la rinomina della directory con il lock vivo, la prova accetta soltanto gli errori 5/32, dimostra che il lock è ancora esclusivo e che la rinomina e la riacquisizione riescono dopo il rilascio; se la rinomina è consentita, resta il percorso della prova sull’ancoraggio. Nessun caso è ignorato.

**Limiti.** L’esito nativo Windows è confermato nel run 46033261562: sei suite passate, inclusa la regressione con lettore aperto; l’esito aggregato resta distinto dal successo di questo job. Non si dichiara un nuovo requisito minimo di Windows né supporto per filesystem non provati. Se il sistema non consente la sostituzione, l’errore resta esplicito e il principale non viene rimosso. Durabilità Windows, login reale e isolamento fra account su Windows/macOS restano non dimostrati.

**Fonti primarie:** [sorgente Rust 1.95.0, funzione rename](https://github.com/rust-lang/rust/blob/1.95.0/library/std/src/sys/fs/windows.rs#L1239), [Microsoft: FILE_RENAME_INFORMATION e semantica POSIX](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information).
