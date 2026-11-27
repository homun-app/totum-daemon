# Modulo `autostart` (D3, passo P9)

**Stato al 27 novembre 2026: libreria implementata e provata su macOS; non collegata all'avvio del binario e senza comandi. P10-A ha aggiunto due prove del backend reale su Windows, compilate per il target Windows ma mai eseguite, e due verifiche dell'unità con `systemd-analyze`, eseguite su macOS solo come salto esplicito per assenza dello strumento; la CI che le eseguirebbe è predisposta ma non è stata avviata.** Il modulo `totum_daemon::autostart` offre tre primitive per account: stato in sola lettura, registrazione idempotente e rimozione esplicita. Non avvia il daemon, non attiva la sessione corrente, non carica servizi e non esegue `launchctl`, `systemctl`, `reg.exe` o altri strumenti. La presenza di una registrazione non dimostra che il daemon sia attivo, caricato o destinato a partire: la verifica di un accesso reale appartiene a un passo successivo. La decisione è registrata in [AD-011](decisions.md).

## Meccanismi

| Meccanismo | Dove | Oggetto | Note |
| --- | --- | --- | --- |
| Launch Agent (macOS) | `$HOME/Library/LaunchAgents` | `app.homun.totum.daemon.plist` | `RunAtLoad` vero, nessun `KeepAlive`, nessun argomento aggiuntivo |
| Unità systemd utente (Linux) | `$XDG_CONFIG_HOME/systemd/user` o `$HOME/.config/systemd/user` | `totum-daemon.service` e `default.target.wants/totum-daemon.service` | Collegamento relativo `../totum-daemon.service`, creato dal codice senza `systemctl` |
| Valore Run (Windows) | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | `app.homun.totum.daemon` | Tipo `REG_SZ`, senza elevazione |

Il nome `app.homun.totum.daemon` è provvisorio come i contratti di lavoro. Ogni meccanismo è per account; non esiste un servizio di sistema, un'installazione con privilegi o un percorso globale.

## API

- `Layout::for_current_user()` rileva il meccanismo e le directory dall'ambiente del profilo. Su Linux la disponibilità di systemd è stimata dall'esistenza di `/run/systemd/system`, la stessa condizione di `sd_booted()`, e viene registrata nel layout al momento della costruzione.
- `Layout::launch_agent(directory)` e `Layout::systemd_user(directory, available)` sono costruttori espliciti per le prove e non vengono usati dal binario.
- `Autostart::new(layout)` combina un layout con le operazioni `status`, `install` e `remove`; le funzioni libere `autostart::status`, `autostart::install` e `autostart::remove` usano `Layout::for_current_user()`.
- `status()` non modifica nulla e restituisce `Registered { program }`, `Absent`, `Incompatible { detail }` o `Unavailable { detail }`.
- `install(&Path)` restituisce `InstallReport { program, entry, link, durability }` oppure `InstallFailure { report, error }`.
- `remove()` restituisce `RemoveReport { entry, link, durability }` oppure `RemoveFailure { report, error }`.

## Stato

| Esito | Significato |
| --- | --- |
| `Registered { program }` | L'oggetto atteso esiste, è posseduto dall'utente, ha protezioni e tipo corretti e il contenuto coincide con la codifica canonica di `program`. |
| `Absent` | L'oggetto o la directory non esistono. |
| `Incompatible { detail }` | Il nome atteso esiste con contenuto, tipo, proprietario, modalità, collegamento o protezioni della chiave `Run` o di un suo antenato diversi. Su systemd, unità presente senza collegamento o collegamento presente senza unità è incompatibile. |
| `Unavailable { detail }` | Il meccanismo non è disponibile: systemd non rilevato su Linux, oppure `RunKey` su una piattaforma non Windows. |

`Registered` descrive soltanto l'oggetto su disco o nel registro: non significa che il sistema lo abbia caricato, che il daemon sia in esecuzione o che partirà al prossimo accesso. `status` non crea directory, non ripara modalità, proprietario o DACL e non segue collegamenti arbitrari.

## Formati e codifica

Il documento prodotto è fisso salvo il percorso del programma. La lettura accetta soltanto un documento identico alla codifica canonica del programma riconosciuto; non esiste una corrispondenza parziale o «quasi propria».

- **Launch Agent.** Il plist contiene `Label` `app.homun.totum.daemon`, `ProgramArguments` con un solo elemento e `RunAtLoad` vero. Il percorso è codificato con le entità XML per `&`, `<`, `>`, `"` e `'`.
- **Unità systemd.** `ExecStart` non passa da una shell: il primo argomento è fra virgolette doppie, preceduto dal prefisso `:` documentato da `systemd.service(5)`, con escape di `\` e `"`. Il prefisso disabilita la sostituzione delle variabili (`$HOME`, `${HOME}` e `$$` restano letterali); i specificatori `%` vengono comunque espansi, quindi una percentuale letterale è codificata come `%%`. Il parser riconosce soltanto la forma con il prefisso. `WantedBy=default.target` completa l'unità.
- **Valore Run.** Il valore è `REG_SZ`, che Windows non espande: `%` e `$` restano letterali e non viene usato `REG_EXPAND_SZ`. Il percorso è fra virgolette solo se contiene spazi o tab, con la codifica degli argomenti di MS C; la riga completa è limitata a 260 unità UTF-16. Una virgoletta doppia nel percorso non è rappresentabile e produce un rifiuto.

Il percorso del binario deve essere assoluto, non la radice, senza separatore finale, senza componenti `.` o `..`, in UTF-8 e senza caratteri di controllo. Non sono ammessi argomenti, variabili, comandi shell, segreti o token. Un percorso non rappresentabile produce `NotRepresentable` prima di qualunque scrittura.

## Registrazione

`install` classifica prima di scrivere e non duplica un oggetto già proprio:

| Situazione | `entry` | Effetto |
| --- | --- | --- |
| Oggetto assente | `Created` | Scrittura di un temporaneo 0600, sincronizzazione e pubblicazione senza sostituzione |
| Oggetto proprio con lo stesso programma | `Unchanged` | Nessuna riscrittura; la durabilità resta `Unknown` |
| Oggetto proprio con un programma diverso | `Updated` | Riclassificazione, confronto con il contenuto letto, temporaneo, sincronizzazione e sostituzione atomica |
| Oggetto estraneo o incompatibile | — | `IncompatibleObject`, nessuna modifica |

Prima della sostituzione l'oggetto viene riclassificato e il contenuto precedente confrontato con quello appena letto: se cambia, l'operazione si ferma con `ChangedDuringRegistration` e non sovrascrive il lavoro altrui. Lo stesso esito vale per un oggetto comparso fra assenza e pubblicazione.

Su systemd l'unità viene scritta prima del collegamento. Il collegamento è proprio se è un collegamento simbolico posseduto dall'utente e il suo testo è esattamente `../totum-daemon.service` oppure il percorso assoluto dell'unità; il testo viene letto senza seguire la destinazione. Non esistendo una sostituzione atomica di collegamento, un collegamento proprio con testo diverso è `IncompatibleObject`. Se il collegamento non può essere creato, `install` restituisce `InstallFailure` con l'oggetto principale già creato o aggiornato e `link: NotAttempted`; un `install` successivo completa soltanto il collegamento.

`Durability` distingue `Unknown` (nessuna scrittura tentata), `Confirmed` (sincronizzazione riuscita) e `NotConfirmed { detail }` (oggetto scritto ma sincronizzazione della directory non riuscita). Un commit non viene annullato da una durabilità non confermata.

## Rimozione

`remove` elimina soltanto oggetti riconosciuti come propri, prima il collegamento e poi l'unità oppure il file o il valore. Un secondo `remove` riporta `AlreadyAbsent` e non è un errore. Un oggetto estraneo o incompatibile non viene mai cancellato. La directory condivisa `default.target.wants` e le directory create dal modulo non vengono rimosse: la rimozione riguarda gli oggetti di Totum.

## Directory e protezioni

Le directory mancanti del percorso vengono create con modalità 0700 e verifica delle ACL. Le directory già presenti non vengono modificate, ma devono appartenere all'utente corrente e non essere scrivibili da gruppo o altri; una protezione errata produce `UnsafeObject` e lascia la directory invariata. Un processo elevato viene rifiutato come già avviene per la radice di stato.

### Protezioni della chiave `Run`

L'appartenenza a `HKCU` e il successo dell'apertura della chiave non dimostrano che altri account non possano modificarla. Prima di ogni scrittura o eliminazione il backend legge proprietario e DACL effettivi dell'handle scrivibile della chiave `Run` e degli antenati `Software`, `Software\Microsoft`, `Software\Microsoft\Windows` e `Software\Microsoft\Windows\CurrentVersion`, e li ricontrolla subito prima della modifica.

Sono ammessi come proprietari l'account corrente, SYSTEM (`S-1-5-18`) e `BUILTIN\Administrators` (`S-1-5-32-544`). La DACL deve esistere; una voce consentita a un altro titolare con diritti di modifica o sostituzione, una voce di tipo non interpretabile o una DACL assente producono `UnsafeObject`. Le voci inherit-only, Denied, Audit e Alarm non concedono diritti. La chiave condivisa non viene riparata e gli altri valori non vengono toccati.

Lo stato usa la stessa politica in sola lettura: una registrazione presente sotto una chiave o un antenato non sicuro è `Incompatible`, con la diagnosi dell'oggetto che viola la politica; una chiave assente resta `Absent` e non valuta protezioni di oggetti inesistenti. `status` non ripara nulla e non duplica le regole delle ACL.

Quando la chiave manca, viene creata con un descrittore di sicurezza esplicito: proprietario l'account corrente, DACL protetta con accesso completo al solo account corrente, SYSTEM e amministratori. La creazione deve restituire `REG_CREATED_NEW_KEY`: una chiave comparsa o scomparsa fra classificazione e scrittura produce `ChangedDuringRegistration`, e il valore viene riconfrontato con lo snapshot prima di essere scritto o eliminato.

## Prove e limiti

Le prove usano `Layout` espliciti con directory temporanee e non scrivono mai `~/Library/LaunchAgents`, `~/.config/systemd/user` o il registro del profilo reale. La disponibilità di systemd è iniettata per provare anche il caso non disponibile su macOS. Sono coperti contenuto e percorso, registrazione ripetuta senza duplicazioni, stato senza modifiche, rimozione ripetuta, collisioni con contenuti estranei, protezioni errate e collegamenti inattesi, errore intermedio senza danno, percorsi con caratteri speciali, assenza di esecuzione di strumenti esterni e assenza di attivazione automatica.

La politica delle protezioni del registro è provata con dati sintetici e un backend di prova: protezioni ammesse, scrittura concessa a un account non fidato, antenato sostituibile, protezioni non leggibili, rimozione non consentita, voci Denied/Audit/Alarm/inherit-only e nessuna modifica dopo un rifiuto. Lo stato è provato separatamente per chiave sicura con antenato non sicuro (`Incompatible` con la diagnosi dell'antenato), catena sicura con valore canonico (`Registered`), protezioni della chiave non sicure (`Incompatible`) e assenza della registrazione (`Absent`, senza ispezione degli antenati quando la chiave manca). Le regressioni systemd coprono `$`, `$$`, `$HOME`, `${HOME}`, percentuali, spazi, virgolette e barre rovesciate, e confrontano il documento con una forma scritta a mano, non soltanto con l'inversione del nostro encoder.

**P10-A, backend reale del registro (Windows).** Due prove di libreria costruiscono una sottochiave univoca `HKCU\Software\Homun\Totum\P10ATests\<pid>-<contatore>`, la eliminano al termine e non toccano la chiave `Run` reale né i suoi valori. La preparazione crea **soltanto gli antenati**: la chiave della prova deve risultare assente, altrimenti la prova fallisce prima di invocare il backend. È il backend a crearla, quindi la prova può affermare `REG_CREATED_NEW_KEY` e non una sua rigenerazione, e può poi controllare la chiave realmente creata: proprietario l'account corrente, DACL presente, bandiera `REG_DACL_SECURITY_PROTECTED` letta da `GetSecurityDescriptorControl` ed esposta in `RegistrySecurity::protected`, e insieme dei fidatari esattamente `{utente corrente}` (o il gruppo amministratori quando il processo è elevato) più SYSTEM e amministratori, letta dall'effettiva `Acl` con `GetAce` e non dalle sole stringhe SDDL. Segue lo stato `Registered`, la registrazione ripetuta `Unchanged` — che è l'unica sede in cui si osserva la chiave già esistente — e la rimozione con assenza successiva. La seconda prova parte da una chiave già creata dal backend con le protezioni di policy e le altera la DACL concedendo a `BUILTIN\Users` il diritto di modifica, così `install` la incontra già esistente: verifica che `status` la segnali `Incompatible` e che `install` e `unregister` la rifiutino. Le protezioni sono rilette prima e dopo i tentativi: se l'alterazione non è applicabile, per esempio una DACL di `HKCU\Software` che già concede il diritto, la prova dichiara il proprio ambiente in un messaggio su standard error e non ripristina nulla; in caso contrario ripristina la chiave e, se il ripristino fallisce, elimina la sottochiave di prova invece di lasciarla con l'ACE estraneo. `RegistrySecurity::protected` è esposta a scopo diagnostico e non partecipa ad alcuna decisione di sicurezza del modulo. Le prove usano soltanto l'API del backend con un percorso interno al test: il backend di produzione resta vincolato a `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` e non espone opzioni, variabili d'ambiente o bypass.

**P10-A, consumatore systemd (Linux).** Una prova d'integrazione installa l'unità in una directory temporanea con un percorso del binario contenente spazi, `$HOME` e `100%`, ne confronta la codifica canonica e la sottopone a `systemd-analyze --man=no verify` quando lo strumento esiste. La verifica è di sola analisi: nessun servizio viene caricato, abilitato o avviato e il modulo applicativo non invoca lo strumento. Su macOS la prova si salta con un messaggio esplicito; in CI `TOTUM_REQUIRE_SYSTEMD_ANALYZE=1` trasforma l'assenza dello strumento in un fallimento.

Il backend del registro Windows è compilato per `x86_64-pc-windows-gnu` e le prove P10-A lo eserciterebbero su Windows, ma non sono state eseguite in questa fase: lettura delle DACL, creazione con descrittore protetto e scrittura in `HKCU` richiedono Windows, e senza push la CI non è partita. Queste prove restano nel job `registry-windows`, che opera su una sottochiave di prova isolata e non tocca il valore `Run` reale; il job separato `windows-non-elevated` esegue le prove di stato con un account effimero non amministratore e le esclude con `--skip real_backend`, perché il registro viene provato soltanto nella chiave del primo. La bandiera `protected` è esposta da `platform` e letta dalle prove, ma non partecipa ad alcuna decisione del modulo, quindi la sua correttezza su un registro reale non è dimostrata. Il target Linux non è installato nella toolchain locale, quindi i blocchi specifici di Linux sono compilati soltanto dalla CI. Il backend systemd è lo stesso codice Unix del Launch Agent e non è stato eseguito su un sistema con systemd reale: non sono verificati `sd_booted()`, il caricamento dell'unità né l'avvio al login. La verifica con `systemd-analyze` è predisposta e saltata su macOS; la semantica del prefisso `:` resta verificata sul sorgente ufficiale di systemd (`config_parse_exec` in `load-fragment.c`, `replace_env_argv` in `exec-invoke.c`). Nessun accesso reale è stato provato. ACL effettive, isolamento fra account e la durabilità alla perdita di alimentazione non sono provati.

## Fuori perimetro

Attivazione della sessione corrente, caricamento o abilitazione dell'unità, verifica di un login reale, servizio di sistema, installer, comandi CLI, collegamento all'avvio del binario, rete, adapter e pairing. Le modifiche alla CI sono limitate ai job di verifica di P10-A e non attivano nulla.
