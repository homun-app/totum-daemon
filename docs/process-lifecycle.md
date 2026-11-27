# Verifica POSIX dell'arresto e dello stato del processo

Il job `process-lifecycle` esegue su macOS e Linux il daemon compilato, prima
con l'utente del runner e poi con un UID numerico differente dal padre root.
Non crea account, non cambia DirectoryService e non registra avvii automatici.
Le home sono sintetiche e vengono rimosse al termine.

## Il rischio del controllo precedente

Un padre che ignora `SIGCHLD` può perdere lo stato del figlio, perché il sistema
lo raccoglie automaticamente. In questo caso `subprocess.Popen.wait` può
restituire 0 quando `waitpid` produce `ECHILD`: quel numero non dimostra
un'uscita ordinaria del daemon. Il comportamento è stato riprodotto localmente
senza privilegi amministrativi: lo stesso binario termina con SIGTERM, ma
l'osservatore perde il motivo se `SIGCHLD` è ignorato.

Il precedente tentativo locale privilegiato non registrava disposizione di
`SIGCHLD`, stato grezzo o uso di SIGKILL. Non si attribuisce quindi con certezza
retroattiva il suo fallimento a questa causa. La nuova verifica rende questi
casi distinguibili e prova l'arresto anche tra UID diversi nell'ambiente CI.

## Casi obbligatori

| Caso | Padre | Esito richiesto |
| --- | --- | --- |
| `baseline` | `SIGCHLD` predefinito | prontezza, stato grezzo disponibile e uscita -15 dopo SIGTERM |
| `ignored` | `SIGCHLD` ignorato intenzionalmente | stato non disponibile; lo 0 del vecchio osservatore è registrato soltanto come controllo negativo |
| `restored` | `SIGCHLD` inizialmente ignorato, poi ripristinato nel padre | stato grezzo disponibile e uscita -15, senza modifiche al daemon |

Ogni caso emette JSON con UID effettivo verificato, disposizione di `SIGCHLD`,
PID, segnale richiesto, disponibilità dello stato, valore grezzo, codice
decodificato e uso dell'eventuale SIGKILL. Un timeout o SIGKILL non sono mai
accettati come arresto riuscito. Lo stato sconosciuto resta `null`, distinto
da un vero codice 0. Il controllo negativo verifica la perdita di stato, non
un arresto valido del prodotto.

L'osservatore usa `waitpid` sul solo figlio lanciato e non assume 0 su `ECHILD`.
Il padre ripristina `SIGCHLD` prima degli avvii monitorati. Non si cambiano
gestori o maschere del daemon per renderlo conforme alla prova.

## Vincoli dell'harness

Il modo root richiede macOS o Linux, eUID 0, `GITHUB_ACTIONS=true`,
`RUNNER_ENVIRONMENT=github-hosted` e `TOTUM_PROCESS_LIFECYCLE_CI=1`, prima di
creare cartelle o lanciare il daemon. Il figlio usa un UID non associato ad
account, senza gruppi supplementari; la home è sua, mentre il contenitore ha
un antenato root non condiviso. Questo evita di rendere non valida la prova
a causa della politica delle directory con sticky bit.

Il modo non privilegiato usa soltanto una directory temporanea dell'utente
corrente. Sono stati verificati i tre casi e una mutazione che rimuove il
ripristino di `SIGCHLD`: la mutazione fallisce su `restored`. Il modo root non
viene eseguito sul Mac personale e si rifiuta fuori dal contesto CI dichiarato.

Il confronto fra UID numerici non sostituisce due account con sessioni reali,
un logout/login grafico o la verifica del LaunchAgent al login. D3/P10 restano
aperti per tali verifiche. Nessun pacchetto, versione SDK, formato o percorso
operativo del prodotto cambia.

Fonti primarie: [CPython 3.9.6, subprocess](https://github.com/python/cpython/blob/v3.9.6/Lib/subprocess.py),
[Python, gestione dei segnali](https://docs.python.org/3/library/signal.html).
