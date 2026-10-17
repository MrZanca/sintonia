# Wireframe schermate principali (v0.2, Web / PC)

Pensati per finestra desktop, uso con mouse e tastiera. Le frecce, Invio ed Esc fanno tutto, così la stessa interfaccia regge anche un telecomando o un PC collegato alla TV.

## Home
```
┌────────────────────────────────────────────────────────────────────────┐
│  ◉ IPTV                                   [Playlist: Casa ▾]   ⚙       │
├──────────┬─────────────────────────────────────────────────────────────┤
│ ▸ Home   │  Continua a guardare                                        │
│   Live   │  [▶ Canale 5   ] [▶ Film: ...  42%] [▶ Serie S2E4  10%]     │
│   Guida  │                                                             │
│   Film   │  Preferiti                                                  │
│   Serie  │  [logo] [logo] [logo] [logo] [logo] [logo]  →               │
│   Cerca  │                                                             │
│          │  Ora in onda sui preferiti                                  │
│          │  Rai 1   20:30 TG1 ▓▓▓▓▓░░░  ·  dopo: Film                  │
└──────────┴─────────────────────────────────────────────────────────────┘
```

## Live (lista canali con anteprima)
```
┌───────────────┬───────────────────────────┬────────────────────────────┐
│ CATEGORIE     │ CANALI                    │  ┌──────────────────────┐  │
│ ★ Preferiti   │ 001 [logo] Rai 1      HD  │  │   anteprima video    │  │
│   Recenti     │ 002 [logo] Rai 2  ◀ focus │  │                      │  │
│ ▸ Italia      │ 003 [logo] Rai 3          │  └──────────────────────┘  │
│   Sport       │ 004 [logo] Rete 4         │  21:00-22:45 Film X        │
│   Cinema      │ ...                       │  Trama breve...            │
│   Bambini 🔒  │                           │  Dopo: 22:45 Notiziario    │
└───────────────┴───────────────────────────┴────────────────────────────┘
  Invio/doppio clic = guarda · F o clic su ★ = preferito · ← → cambia colonna · Ctrl+F = cerca
```

## Guida EPG (griglia)
```
┌──────────────┬─────────┬─────────┬─────────┬─────────┬─────────────────┐
│ Ven 17 ott   │ 20:00   │ 20:30   │ 21:00   │ 21:30   │ 22:00           │
├──────────────┼─────────┴─────────┼─────────┴─────────┴─────────────────┤
│ 001 Rai 1    │ TG1 · Soliti Ign. │ Film: titolo                        │
│ 002 Rai 2    │ Serie A ep.3      │ Talk show          │ Notizie        │
│ 003 Rai 3    │ Blob │ Un posto al sole │ Report                       │
└──────────────┴───────────────────┴─────────────────────────────────────┘
         │ linea "adesso"     Invio su un programma = guarda / dettagli / catch-up
```

## Player (overlay)
```
┌────────────────────────────────────────────────────────────────────────┐
│                                                                        │
│                          (video a schermo intero)                      │
│                                                                        │
│ ┌────────────────────────────────────────────────────────────────────┐ │
│ │ 002 [logo] Rai 2                           21:14   ● LIVE   1080p   │ │
│ │ Serie A ep.3   ▓▓▓▓▓▓▓▓▓▓▓░░░░░░░  20:30-21:30                     │ │
│ │ Dopo: Talk show                    [Audio] [Sub] [16:9] [Info]      │ │
│ └────────────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────────────┘
  ↑ ↓ zapping · Spazio pausa · F schermo intero · Esc torna alla lista · ← → lista canali laterale
```

## Aggiunta playlist
```
┌──────────────────────────────────────────────┐
│  Aggiungi sorgente                           │
│  ( ) Link M3U  (•) Xtream Codes  ( ) File ⇪ │
│                                              │
│  Nome         [ Casa                    ]    │
│  Server       [ http://...:8080         ]    │
│  Utente       [ ********                ]    │
│  Password     [ ********                ]    │
│  EPG (opz.)   [ automatico dal server   ]    │
│                                              │
│            [ Verifica ]   [ Salva ]          │
└──────────────────────────────────────────────┘
```

## Dettaglio film / serie
```
┌──────────┬─────────────────────────────────────────────────────────┐
│          │ Titolo (2024) · 2h 05m · ★ 7.4                         │
│ locandina│ Trama...                                                │
│          │ [▶ Riprendi da 48:12]  [Dall'inizio]  [★]              │
├──────────┴─────────────────────────────────────────────────────────┤
│ Stagione 1 ▾   E1 ✓   E2 ✓   E3 ▓▓░░   E4   E5 ...                 │
└────────────────────────────────────────────────────────────────────┘
```
