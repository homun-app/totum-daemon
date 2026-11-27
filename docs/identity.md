# Identità iniziale e registro dei controller

Stato al 27 novembre 2026: **libreria implementata e provata su macOS; usata dal percorso operativo del daemon dal passo P8**. Il modulo `totum_daemon::identity` (D3, passo P7) classifica `identity.json` e `controllers.json`, espone gli stati tipizzati richiesti dalla coerenza della configurazione e completa la sola prima inizializzazione mancante sotto il lock esclusivo della radice. Dal passo P8 `totum-daemon` chiama `initialize` e poi verifica la coerenza prima di aprire `config::Store`; `totum` conserva il comportamento D1.

## Perimetro

Il modulo rappresenta soltanto lo stato persistito iniziale:

- `identity.json` contiene lo stato `notInitialized`;
- `controllers.json` è il contenitore vuoto del registro, con `grantRevision` pari a 0.

Non esistono chiavi, impronte, token, coppie, certificati o altro materiale crittografico; il modulo non ne genera e non ne cerca. Non esistono pairing, autorizzazione dei controller, modifica dei grant, revoca o `resetIdentity`. Lo schema delle future voci di `controllers` non è definito e non viene inventato: una voce presente è un contenuto non iniziale, non un formato interpretabile. Le decisioni di formato sono in [decisions.md](decisions.md) (AD-009).

## Formati versionati

Entrambi i documenti sono oggetti JSON UTF-8 con `schemaVersion` pari a 1 e terminano con un solo a capo. La serializzazione produce esattamente:

```json
{"schemaVersion":1,"state":"notInitialized"}
{"schemaVersion":1,"grantRevision":0,"controllers":[]}
```

Il primo è `identity.json`, il secondo `controllers.json`.

Regole comuni:

- il documento deve essere un oggetto JSON; un array, un valore scalare o una sequenza posizionale sono rifiutati anche quando la libreria di deserializzazione li accetterebbe;
- i campi sono esattamente quelli indicati, con i tipi indicati: `schemaVersion` e `grantRevision` sono interi senza segno, `state` è una stringa, `controllers` è un array;
- campi mancanti, sconosciuti o duplicati, documenti troncati, testo dopo il documento e tipi diversi sono `InvalidDocument`;
- una `schemaVersion` diversa da 1 è uno schema non compreso (`UnsupportedSchema`): in P7 non esiste alcuna migrazione dei file di identità e nessuna versione viene reinterpretata;
- uno stato diverso da `notInitialized`, una `grantRevision` diversa da 0 o un array `controllers` non vuoto sono contenuti **validi ma non iniziali** (`NonInitial`): non vengono azzerati, corretti né riscritti;
- i documenti non contengono un campo di revisione. A differenza di `config.json`, che numera le generazioni per il backup e il recupero, i file di identità non hanno generazioni proprie: la sola transizione ammessa in D3 è assente → iniziale, quindi una revisione non avrebbe un uso verificabile (AD-009).

## Lettura e classificazione

`identity::classify(root)` richiede una `VerifiedRoot` già verificata e ancorata da `platform::prepare_root` e non modifica nulla:

- il percorso della radice viene ricontrollato con `VerifiedRoot::verify_path` prima di aprire i file: una rinomina o una sostituzione producono `Protection(PathChanged)`, mai un'assenza;
- i file si aprono con `VerifiedRoot::open_file`, che rifiuta collegamenti simbolici e reparse point e verifica tipo regolare, proprietario, modalità esattamente 0600 e ACL su Unix o DACL protetta su Windows, senza riparazioni;
- solo `PlatformError::FileAbsent` diventa assenza; ogni altro errore è un rifiuto e non viene interpretato come file mancante;
- la lettura è limitata a 64 KiB per file (`MAX_IDENTITY_BYTES`); un file più grande è `TooLarge` e non viene troncato né interpretato.

Ogni file produce un'osservazione:

| Osservazione | Significato | `ComponentState` per la configurazione |
| --- | --- | --- |
| `Absent` | il file non esiste nella radice verificata | `Absent` |
| `Initial` | documento valido con il solo contenuto iniziale | `Initial` |
| `NonInitial` | documento valido ma non iniziale | `NonInitial` |
| `Unusable` | documento non valido, schema non compreso, oltre il limite o protezione rifiutata | `Unusable` |

`Classification::coherence()` converte le due osservazioni nel tipo `config::CoherenceInput`. L'assenza resta distinta da un file iniziale presente: la configurazione rifiuta una propria presenza quando uno dei due componenti è `Absent`. La classificazione del modulo è in sola lettura e può essere chiamata senza lock; la prima inizializzazione richiede il lock e un accesso esclusivo alla radice.

## Prima inizializzazione

`identity::initialize(locked)` richiede **`&mut platform::LockedRoot`**, l'oggetto che possiede insieme la radice ancorata e il lock esclusivo, e lo trattiene per tutta l'operazione. Non esiste una variante senza lock né una variante con accesso condiviso: due chiamate concorrenti sulla stessa radice non compilano, mentre due radici diverse restano indipendenti e non esiste alcun lock globale. L'esclusività è statica, verificata da un doctest `compile_fail`: la pulizia dei temporanei non può cancellare il temporaneo attivo di un'altra chiamata. Dopo l'inizializzazione il `LockedRoot` può essere trasferito a `config::Store`. Per ogni ciclo:

1. classifica `identity.json` e `controllers.json` con le regole precedenti;
2. classifica `config.json` e `config.json.prev` con `config::classify`, passando la coerenza dei due componenti;
3. applica la tabella seguente;
4. soltanto se la decisione ammette la creazione, elimina i temporanei del namespace `.identity.` e pubblica i file mancanti, prima `identity.json` e poi `controllers.json`.

| Identità | Registro | Configurazione e backup | Esito |
| --- | --- | --- | --- |
| iniziale | iniziale | assenti o in qualunque stato non rifiutato | nessuna scrittura; esito riuscito con `created` vuoto |
| assente | assente | entrambi assenti (prima esecuzione) | crea entrambi i file |
| assente | iniziale | entrambi assenti | crea `identity.json` |
| iniziale | assente | entrambi assenti | crea `controllers.json` |
| assente o iniziale | assente o iniziale, almeno uno assente | principale o backup presente | `IncoherentState` con il componente mancante; nessuna scrittura |
| non iniziale | qualunque | qualunque | `NonInitial`; nessuna correzione |
| inutilizzabile | qualunque | qualunque | l'errore di classificazione; nessuna scrittura |
| qualunque | qualunque | rifiutata da `config::classify` | `ConfigRefused`; nessuna scrittura |

La configurazione non viene mai creata da questo modulo: la sua prima scrittura resta al passo P8, quando lo `Store` verrà aperto. Il modulo non applica recuperi né migrazioni della configurazione e non tocca `config.json`, `config.json.prev` o `config.corrupt.*`. Un file iniziale già presente non viene mai riscritto, ricreato o duplicato.

### Ordine, punti di commit e risultati parziali

I due file non sono una coppia atomica. Ogni file viene:

1. scritto in un temporaneo esclusivo `.identity.<pid>.<contatore>.tmp` con modalità 0600 imposta dalla piattaforma;
2. sincronizzato;
3. pubblicato con una rinomina che **non sostituisce** la destinazione; la pubblicazione riuscita è il punto di commit di quel file;
4. seguito dalla sincronizzazione della directory.

Conseguenze dichiarate:

- un errore prima della pubblicazione restituisce `InitError` con `created` vuoto per quel file e nessun documento visibile; il temporaneo viene rimosso dal guard;
- un arresto o un errore fra i due file lascia uno stato parziale: se `identity.json` è stato pubblicato e `controllers.json` no, il riavvio vede identità iniziale e registro assente, con configurazione assente, e crea soltanto il registro; `Startup::created` elenca soltanto i file pubblicati da quella chiamata;
- dopo il commit la sincronizzazione della directory può non essere supportata o fallire: l'esito per quel file è `Commit::CommittedNotDurable`, il file resta pubblicato e la chiamata non viene ritentata automaticamente. Non è un errore e non annulla il commit;
- se la destinazione compare fra la classificazione e la pubblicazione (per esempio per un altro scrittore che ignora il lock), la rinomina fallisce con `AlreadyExists` e il file non viene sovrascritto; la classificazione viene ripetuta, al massimo tre cicli (`MAX_INIT_ATTEMPTS`), e se il conflitto persiste l'esito è `AppearedDuringInitialization` con il percorso interessato;
- la durabilità alla perdita di alimentazione non è dimostrata: un arresto simulato di un processo non equivale a una caduta della macchina.

I temporanei del namespace `.identity.` sono un residuo del modulo: con il lock esclusivo e l'accesso `&mut` richiesto da `initialize` nessun'altra chiamata o istanza cooperante scrive nella stessa radice, quindi ogni nome conforme viene rimosso prima delle scritture, indipendentemente dal PID. I nomi fuori dal namespace, compresi `.config.*`, `config.corrupt.*`, i file dell'utente e i nomi simili, restano intatti. La rimozione avviene soltanto in una chiamata che procede oltre la classificazione; un rifiuto lascia il contenuto com'è.

## Errori

`IdentityError` distingue lettura fallita, protezione rifiutata, file oltre il limite, schema non compreso, documento non valido, contenuto non iniziale, stato incoerente, rifiuto della configurazione, serializzazione, scrittura non commessa, collisione dei nomi temporanei e destinazione comparsa. Ogni variante nomina il file o il componente e nessuna produce un valore predefinito, un'identità generata o una riparazione.

`InitError` contiene l'errore e i file già pubblicati (`created`). Un `created` non vuoto è l'unico modo per sapere che la chiamata ha reso visibili scritture prima del guasto; un `created` vuoto garantisce che questa chiamata non ha pubblicato nulla.

## Lock e concorrenza

Il lock è lo stesso di `platform::lock`: consultivo, senza attesa e condiviso con `config::Store`. Se un'altra istanza lo detiene, `initialize` non viene nemmeno chiamata perché `platform::lock` restituisce `AlreadyRunning`; la seconda istanza non legge né modifica identità, registro, configurazione o temporanei. All'interno dello stesso processo l'esclusività è garantita dalla firma `&mut LockedRoot`: non esiste un percorso che condivida la radice fra due chiamate, e il compilatore rifiuta il tentativo. La contesa residua è quella di un processo che ignora il lock: la classificazione viene ripetuta e la pubblicazione non sostituisce mai una destinazione comparsa, ma i suoi temporanei non sono protetti. Dopo l'inizializzazione la stessa radice può essere trasferita a `config::Store`, che ne assume la proprietà.

## Prove eseguite

Le prove della libreria coprono:

- parsing e serializzazione: oggetti, duplicati, campi sconosciuti o mancanti, tipi errati, documenti troncati, schema futuro e schema 0, stato non iniziale, revisione non zero, registro non vuoto, byte esatti dei documenti iniziali;
- creazione: byte e modalità 0600 del file pubblicato, nessun temporaneo residuo, mancata sovrascrittura di una destinazione preesistente o comparsa, pulizia limitata al namespace, collisioni dei nomi, guasti in ogni passo e `CommittedNotDurable` dopo il commit;
- esclusività statica: un doctest `compile_fail` con codice `E0308` dimostra che due chiamate concorrenti sullo stesso `&LockedRoot` non compilano; non esiste un mutex e radici indipendenti non si bloccano;
- arresto del processo in quattro punti: prima della pubblicazione dell'identità, dopo quella pubblicazione, prima e dopo la pubblicazione del registro; il riavvio completa soltanto i file mancanti e non riscrive quelli presenti;
- il padre riconosce l'arresto richiesto dalla piattaforma, cioè `SIGABRT` su Unix e il codice `0xC0000409` di `__fastfail(FAST_FAIL_FATAL_APP_EXIT)` su Windows; un'uscita ordinaria, un panico o il fallback `exit(17)` non sono accettati. Una prova dedicata avvia il fallback e verifica che venga rifiutato;
- integrazione con la configurazione: assenza distinta dallo stato iniziale, configurazione o backup presenti con un componente mancante, configurazione o backup coerenti con entrambi i componenti iniziali, configurazione corrotta o futura, prima esecuzione con `Store::open`, seconda istanza esclusa dal lock, nessun file di chiavi o segreti.

## Limiti

- Le prove sono state eseguite soltanto su macOS. Le prove d'integrazione compilano per `x86_64-pc-windows-gnu` e sono assegnate al job `windows-non-elevated`, che nella CI del 27 novembre 2026 si è fermato prima delle prove, e nessuna esecuzione Windows o Linux delle prove di stato è stata fatta; i reparse point e il riconoscimento Windows dell'arresto (`0xC0000409`) sono verificati soltanto in compilazione. La matrice `rust` non esegue le prove di stato su Windows, dove il runner è amministratore.
- ACL effettive, isolamento fra account e un `identity.json` posseduto da un altro account richiedono privilegi o un secondo account e non sono stati provati. Un file con proprietario errato è un rifiuto per costruzione, non una prova eseguita.
- La durabilità alla perdita di alimentazione non è provata.
- Il lock è consultivo e non copre processi che lo ignorano.
- Non esistono migrazioni dei file di identità, recupero di un file corrotto, backup di identità o cancellazione di `controllers.json`. Un contenuto non iniziale è un rifiuto in D3.
- Il collegamento all'avvio è stato aggiunto da P8: `totum-daemon` completa i soli file iniziali mancanti, riclassifica la coerenza e solo allora apre `config::Store`, senza riscrivere file presenti e senza comandi che pilotino l'inizializzazione.
