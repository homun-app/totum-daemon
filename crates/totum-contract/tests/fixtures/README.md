# Fixture dei contratti A/C

Dati interamente sintetici, senza percorsi, nomi di progetto o identificatori
reali. Le fixture sono contratti di lavoro rivedibili, non schemi stabili.

## `valid/`

Un esempio per ogni messaggio supportato dalle superfici A e C. I campi
verificati dai test sono i valori decodificati, non soltanto il round-trip del
serializzatore. `adapter-hello.json` dichiara anche `futureCapability`, che
resta un dato valido ma non riconosciuto dalla negoziazione.

## `invalid/`

Input che la decodifica deve rifiutare senza panico: superficie e tipo
sconosciuti, campo obbligatorio mancante, tipo errato, JSON malformato,
carattere di controllo nel testo, impronta non valida, correlazione mancante o
non ammessa, intervallo di versioni invalido, capability duplicata,
`hubHello` che riconosce una capability sconosciuta a questa revisione.

I casi fuori limite (65 capability, testo oltre 4096 caratteri, messaggio oltre
64 KiB) sono costruiti nei test per restare leggibili.

## `incompatible/`

`adapter-hello-v2.json` dichiara la maggiore 2.0: la decodifica la rifiuta come
incompatibile e non la considera compatibile per via della sola minore.
