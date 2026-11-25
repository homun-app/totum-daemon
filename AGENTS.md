# Istruzioni Totum Daemon

- Questo repository è pubblico e riguarda soltanto il daemon Rust per computer macOS, Windows e Linux.
- Fase corrente: fondazione documentale. Non creare crate, implementazioni, installer o release senza una successiva autorizzazione applicativa.
- L’app mobile e il resto del prodotto sono privati. Non copiare sorgenti, design system, documentazione interna o file da repository privati nel presente repository.
- Non leggere o pubblicare il prototipo visivo dell’app per popolare questo repository. Nessun asset, screenshot, archivio o inventario del prototipo è ammesso qui.
- Segreti, chiavi di firma, dati reali degli agenti e credenziali non entrano in Git né nei log pubblici di Actions.
- Prima dell’implementazione definire interfacce minime che consentano una build del daemon senza accesso a sorgenti privati; non ampliare automaticamente il perimetro pubblico a protocollo, adapter o relay.
- Documentare capacità e piattaforme come obiettivi finché non sono verificate. Nessuna integrazione agente è attualmente disponibile.
- Documenti in italiano, identificatori tecnici in inglese. Tagline: `All your agents. One place.`
- Prima di un commit: `git diff --check` e revisione di tutti i file da pubblicare.
- Non assegnare una licenza senza una decisione esplicita del proprietario.
