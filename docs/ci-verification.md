# Evidenze CI e correzioni dell'harness

Stato al 27 novembre 2026. D3/P10 restano aperti; una suite compilata, avviata o andata in timeout non equivale a una suite superata.

## Esecuzioni osservate

| Commit e run | Esito | Evidenze |
| --- | --- | --- |
| `473ab73`, [46010768568](https://github.com/homun-app/totum-daemon/actions/runs/46010768568) | 6 job verdi, 4 falliti | Registro reale Windows, verificatore systemd, TB-08 Linux con due account reali, self-test TB-08 su macOS/Linux e whitespace passati. Matrice Rust fermata da tipo umask non portabile e dipendenze mancanti per i metadati offline. Account Windows non creato per descrizione troppo lunga. |
| `70e064c`, [46020171656](https://github.com/homun-app/totum-daemon/actions/runs/46020171656) | 8 job verdi, 2 falliti | Matrice Rust Linux e Windows passate; riconfermati tutti i sei job già verdi. macOS fallisce nella libreria per lock intermittente. Il job Windows dedicato verifica account, SID, token non elevato e scrittura, poi va in timeout nella suite `config`; fallisce anche la rimozione dell'albero. |

Nel secondo run il rifiuto del daemon con token elevato è passato: uscita 1, diagnosi esplicita e nessuna radice creata. Il processo dell'account effimero ha riportato `tokenElevation=0`, `administratorRole=False` e `writeCheck=ok`. Le suite Windows successive a `config` non sono state raggiunte.

## Correzione locale dei test macOS

Il fallimento `AlreadyRunning` in `migration_failures_before_the_commit_preserve_the_original_generation` è stato riprodotto alla settima ripetizione della libreria. Le transizioni rilascio-riacquisizione di `config` erano coordinate soltanto con gli avvii dei propri figli: gli avvii di `identity` erano fuori dal mutex. Un figlio può trattenere il descrittore ereditato fino a `exec`; la semantica del lock è già coperta dalla prova sul descrittore duplicato.

Il modulo privato `test_support`, compilato soltanto con `cfg(test)`, condivide ora la coordinazione fra entrambi i moduli. Il mutex protegge soltanto lo spawn e le transizioni interessate, non l'intera durata dei figli o l'intera suite. Nessuna modifica al lock operativo e nessun retry.

La nuova regressione sullo spawn è fallita senza la protezione ed è passata con essa. Dopo la correzione sono passate 30 esecuzioni consecutive della libreria con il parallelismo predefinito, oltre alla suite completa. La libreria macOS conta ora 105 test: 103 eseguiti e due helper ignorati, avviati dalle prove di arresto.

## Diagnostica e pulizia Windows da verificare in CI

Il run non consente di attribuire il timeout a un test specifico: `Invoke-AsUser` sollevava l'eccezione prima di restituire i percorsi dei log al chiamante. La causa del blocco resta quindi sconosciuta.

L'harness ora:

- usa un'attesa comune e limitata per processi del runner e dell'account di prova;
- stampa stdout/stderr anche al timeout, con PID, CPU, handle e nomi dei discendenti prima della terminazione; non legge ambiente o righe di comando;
- riporta avanzamento ogni 30 secondi e usa i nomi dei test e `--nocapture`; il limite di 600 secondi non è aumentato;
- mostra l'esito della terminazione del processo;
- prima di cancellare l'account, termina i suoi processi e rimuove il solo sottoalbero `src` con quel token. Le radici di stato hanno DACL per il solo proprietario: non si presume che il token del runner possa cancellarle. Le ACL operative non vengono indebolite;
- continua i tentativi separati di rimozione di account, profilo e albero anche se la pulizia della sorgente fallisce.

`windows-harness-selftest.ps1` carica soltanto le definizioni di funzione, senza eseguire il corpo operativo. In locale ha verificato con processi reali timeout e conservazione di entrambi i flussi, uscita 7 conservata, e rimozione del solo sottoalbero sintetico con spazi e apostrofi nel percorso. Per quest'ultima prova sono simulati esclusivamente cambio di account e arresto per nome utente: non dimostra DACL o pulizia Windows reale. Il test del timeout è fallito prima della correzione ed è passato dopo. Il workflow esegue questo self-test prima dell'harness Windows.

La verifica locale ha usato una distribuzione portabile ufficiale PowerShell in una directory temporanea esterna al repository, senza installazione di sistema. Nessun account locale è stato creato. Restano da rieseguire in CI il ramo Windows di diagnostica e terminazione, le prove di stato e la pulizia completa.

## Verifiche locali

Sono passati formato, Clippy con warning come errori, suite completa e build con lockfile e modalità offline su macOS, oltre a Clippy per `x86_64-pc-windows-gnu`. La compilazione incrociata non esegue i test Windows. Le due prove systemd che ritornano su macOS non costituiscono evidenza systemd locale; l'evidenza reale proviene dalla CI Linux.

Restano aperti login reale, isolamento fra account su macOS e Windows, casi manuali del proprietario estraneo e durabilità alla perdita di alimentazione. Nessuna attivazione dell'avvio automatico, release o deployment è inclusa in queste correzioni.
