# Evolution GTK4 — ausführbare Entwicklungsroadmap

Diese Datei ist die operative Warteschlange für Menschen und LLM-Agenten.
Die vollständige Produktspezifikation und der belegte Ist-Stand bleiben in
[ROADMAP.md](ROADMAP.md). Ausschließlich auf Linux prüfbare Arbeiten stehen in
[ROADMAP_LINUX.md](ROADMAP_LINUX.md).

## 1. Dokumentrollen

- `ROADMAP.md` beantwortet: Welche Evolution-Funktionen und Einstellungen
  müssen am Ende vorhanden sein, und was ist bereits belegt?
- `ROADMAP_EXECUTION.md` beantwortet: Welche atomare Arbeit ist als Nächstes
  auszuführen, welche Dateien sind betroffen und welcher Beleg schließt sie ab?
- `ROADMAP_LINUX.md` beantwortet: Welche native EDS-, Provider-, Desktop- oder
  Paketabnahme kann nur auf einem Linux-System erfolgen?
- `AGENTS.md` und die untergeordneten DOX-Dateien beantworten: Welche dauerhaften
  Arbeits- und Architekturverträge gelten beim Ändern des Projekts?

Keines dieser Dokumente ersetzt ein anderes. Ein Ausführungspunkt darf den
Produktumfang aus `ROADMAP.md` nicht verkleinern.

## 2. Ausführungsvertrag für LLM-Agenten

Vor jeder Implementierung:

1. `AGENTS.md` und alle für die Zielpfade geltenden untergeordneten
   `AGENTS.md`-Dateien vollständig lesen.
2. Den aktuellen Worktree prüfen. Bereits vorhandener Code und Tests sind
   autoritativer als ältere Fortschrittstexte.
3. Den ersten `READY`-Punkt mit der höchsten Priorität wählen, dessen
   Abhängigkeiten erfüllt sind und dessen Plattform verfügbar ist.
4. Einen Punkt vor dem Codieren weiter aufteilen, wenn er nicht in einem
   zusammenhängenden, kompilierbaren Commit mit eindeutigem Fehlerpfad und
   Tests abgeschlossen werden kann.
5. Die historische GTK3-Version ausschließlich als Referenz für sichtbares
   Verhalten, Menüstruktur, Tastaturbedienung, Dichte und Arbeitsabläufe lesen.
   EDS/Camel/ECal/EBook bleiben die autoritativen Daten-Backends.

Während der Implementierung:

- Kein UI-Platzhalter, Toast oder vorhandener Button gilt als Funktion.
- Netzwerk-, EDS-, Camel-, ECal-, EBook-, Datei- und Kryptografiearbeit darf
  den GTK-Hauptthread nicht blockieren.
- Eine Benutzereinstellung ist erst fertig, wenn sie persistent ist, beim
  Start geladen wird und eine bereits geöffnete Ansicht live aktualisiert.
- Eine Mutation ist erst fertig, wenn Erfolg, Fehler, Konflikt, Abbruch und
  der sichtbare Zustand danach konsistent sind.
- Portable Fallbacks dürfen synchronisierte EDS-Daten nicht als erfolgreich
  ersetzt ausgeben.

Vor `DONE`:

1. Alle Abnahmekriterien des Punktes mit direkter Evidenz prüfen.
2. Formatierung, gezielte Tests und den Workspace-Build ausführen.
3. Bei querschnittlichen Änderungen die vollständige Testsuite ausführen.
4. `ROADMAP.md` nur anhand tatsächlich verbesserter Produktbelege ändern.
5. Linux-only Restarbeiten mit einer konkreten Testanleitung in
   `ROADMAP_LINUX.md` verknüpfen; sie auf macOS niemals als bestanden markieren.
6. Den Evidenzblock mit Commit, Tests und verbleibenden Einschränkungen füllen.

## 3. Status- und Prioritätsmodell

Statuswerte:

- `READY`: ohne bekannte Vorbedingung auf der angegebenen Plattform startbar.
- `IN PROGRESS`: Implementierung oder Integration begonnen; Tests und/oder
  Abnahmebelege fehlen noch. Mehrere Punkte dürfen diesen Status nur bei
  disjunkten Schreibbereichen tragen.
- `BLOCKED_LINUX`: alle portablen Abnahmekriterien sind erfüllt; nur die
  reale Linux-Abnahme fehlt. Ein Punkt mit noch offenen portablen Tests bleibt
  `IN PROGRESS`.
- `DONE`: alle Abnahmekriterien und Belege liegen vor.
- `SUPERSEDED`: durch konkret genannte Nachfolgepunkte ersetzt.

Prioritäten:

- `P0`: täglicher Mailbetrieb, Datenintegrität, Hänger, Abstürze und Recovery.
- `P1`: täglich genutzte Groupware-Funktionen und Workflow-Parität.
- `P2`: erweiterte Regeln, Enterprise-, Sicherheits- und Migrationsparität.
- `P3`: vollständige Einstellungs-, Accessibility-, Packaging- und
  Release-Abnahme.

Ein übergeordneter Punkt bleibt offen, solange ein daraus abgeleiteter
Pflichtpunkt offen ist. Prozentwerte werden nicht aus Zeilenzahlen geschätzt;
Fortschritt wird nur über abgeschlossene Abnahmekriterien berichtet.

## 4. Aktuelle Ausführungsreihenfolge

Die erste `READY`-Zeile ohne offene Abhängigkeit ist der Standard für den
nächsten Entwicklungsblock. Mehrere Zeilen dürfen parallel laufen, wenn ihre
Schreibbereiche disjunkt sind. Linux-Punkte werden übersprungen, solange kein
Linux-System verfügbar ist.

Eltern-IDs wie `P0-MAIL-001` beschreiben ein Produktziel. Ausführbare Pakete
tragen einen Buchstabensuffix. Ein Agent bearbeitet genau ein solches Paket
und erweitert dessen Schreibbereich nur nach dokumentierter Kollisionsprüfung.

| Reihenfolge | Paket | Elternziel | Primärer Schreibbereich | Abhängigkeit | Status | Commit-Abnahme |
|:---:|:---|:---|:---|:---|:---:|:---|
| 1 | `P0-STAB-001A` | `P0-STAB-001` | `src/services/mail_operations_service.rs` | keine | DONE | Lebenszyklus, Fehler, Konflikt und Abbruch getestet |
| 2 | `P0-MAIL-001A` | `P0-MAIL-001` | `src/services/mail_store_service.rs` | keine | DONE | MIME-Korpus und Parser-Fallbacks grün |
| 3 | `P0-MAIL-001B` | `P0-MAIL-001` | `src/services/html_sanitizer.rs`, `src/services/inline_content_service.rs` | keine | DONE | sichere Layout-CSS und Remote-Policy getestet |
| 4 | `P0-COMP-001A` | `P0-COMP-001` | `src/services/mail_composer_service.rs` | keine | DONE | RFC822-/MIME-Korpus einschließlich Fehlerfällen grün |
| 5 | `P1-SEARCH-001A` | `P1-SEARCH-001` | `src/services/search_engine.rs` | keine | DONE | Suchsyntax und ungültige Eingaben deterministisch getestet |
| 6 | `P1-CAL-001A` | `P1-CAL-001` | `src/services/recurrence_service.rs` | keine | DONE | Recurrence-Ausnahmen und Grenzfälle getestet |
| 7 | `P1-CONTACT-001A` | `P1-CONTACT-001` | `src/services/vcard_service.rs` | keine | DONE | verlustarmer vCard-Roundtrip getestet |
| 8 | `P2-MIGRATION-001A` | `P2-MIGRATION-*` | `src/services/migration_service.rs` | keine | DONE | portable Werte, Warnungen und Geheimnis-Redaktion getestet |
| 9 | `P0-STAB-001B` | `P0-STAB-001` | `src/views/mail_page.rs` | `P0-STAB-001A` | DONE | Einzel/Batch/Undo/Redo asynchron, abbrechbar und bestätigungsbasiert |
| 10 | `P0-MAIL-001C` | `P0-MAIL-001` | `src/views/mail_viewer.rs` | `P0-MAIL-001A`, `P0-MAIL-001B` | DONE | Viewer-Fallback und nachrichtenbezogene Remote-Freigabe integriert |
| 11 | `P0-MAIL-001D` | `P0-MAIL-001` | `src/views/mail_viewer.rs`, `src/views/mail_page.rs`, `src/models/mail_message.rs`, `src/services/inline_content_service.rs` | `P0-MAIL-001A`, `P0-MAIL-001B` | BLOCKED_LINUX | Latest-Wins-Worker, CID/Anlagen ohne GTK-Kopie, Cache-/Stale-Gates grün; WebKitGTK-Churn-Abnahme offen |
| 12 | `P0-MAIL-002A` | `P0-MAIL-002` | `src/views/mail_page.rs`, `src/style.css` | `P0-STAB-001B` | IN PROGRESS | kompakter Evolution-Ordnerbaum ohne Kontoicon, lesbare Tabellenschrift, Icon-Fallbacks und dezente Unread-Zähler |
| 13 | `P0-MAIL-002B` | `P0-MAIL-002` | `src/views/mail_page.rs`, `src/services/settings_service.rs`, `src/dialogs/preferences_dialog.rs`, `data/org.gnome.Evolution4.gschema.xml` | `P0-MAIL-002A` | BLOCKED_LINUX | Defaults/Roundtrip/Reset und Live-Bindings portabel grün; sichtbare Linux-Abnahme offen |
| 14 | `P0-SYNC-001A` | `P0-SYNC-001` | `src/views/mail_page.rs`, `src/services/camel_mail_service.rs`, `src/services/exchange_service.rs`, `src/models/mail_message.rs` | `P0-STAB-001B` | BLOCKED_LINUX | Portable Paging-, Watcher-, Auswahl-, Sortier- und echter Graph-Abbruchvertrag grün; Camel-Abnahme offen |
| 15 | `P0-COMP-001B` | `P0-COMP-001` | `src/dialogs/compose_dialog.rs`, `src/services/draft_store_service.rs` | `P0-COMP-001A` | DONE | Autosave, Recovery, pro-Draft-Sendelock und revisionssicheres Cleanup integriert |
| 16 | `P0-RECOVERY-001A` | `P0-RECOVERY-001` | `src/services/eds_service.rs`, `src/services/eds_source_cache_service.rs` | Pakete 9–15 | DONE | Offline/Online, persistenter Backoff, atomarer Cache und Neustart portabel getestet |
| 17 | `P1-CAL-001B` | `P1-CAL-001` | `src/views/calendar_page.rs` | `P1-CAL-001A` | DONE | Ansichten verwenden gemeinsame Recurrence-/Zeitzonenregeln |
| 18 | `P1-CONTACT-001B` | `P1-CONTACT-001` | `src/views/contacts_page.rs` | `P1-CONTACT-001A` | DONE | kompakter quellenstabiler Kontaktworkflow und Dublettenpfad |
| 19 | `P1-TASK-001A` | `P1-TASK-001` | `src/views/tasks_page.rs` | keine | DONE | Quick-Edit, Filter, Suche, Sortierung und Fehlerzustände integriert |

### Parallelisierungsregeln

- Genau ein Agent schreibt gleichzeitig in einen Pfad. Überschneiden sich die
  primären Schreibbereiche, werden die Pakete seriell integriert.
- Jeder Agent liest die geltende DOX-Kette, ändert weder Roadmaps noch
  `AGENTS.md`, erstellt keinen Branch und committet oder pusht nicht selbst.
- Der Hauptagent behält den kritischen UI-Pfad, prüft Agentenänderungen einzeln,
  führt nach jeder Integration gezielte Tests und am Ende den Workspace-Test
  aus und aktualisiert erst danach Evidenz und Status.
- Ein Agentenbericht nennt geänderte Pfade, erfüllte Abnahmekriterien,
  ausgeführte Tests und bekannte Restarbeit. Reine Analyse schließt kein Paket.
- Ein fehlender Agentenslot blockiert die Warteschlange nicht; der Hauptagent
  bearbeitet dann das erste startbare Paket selbst.

## 5. P0 — täglicher Mailbetrieb und Stabilität

### P0-STAB-001 — Mailmutationen vom GTK-Hauptthread trennen

- Status: `IN PROGRESS`
- Quelle: `ROADMAP.md`, Phase 1 „Mailaktionen“ und Phase 7 „Performance“
- Problem: Manuell ausgelöste Backendaktionen verwenden teilweise noch einen
  synchronen Command-Pfad aus GTK-Callbacks. Authentifizierung, Netzwerk oder
  ein langsamer Provider kann dadurch das Fenster als nicht reagierend zeigen.
- Lieferumfang:
  - Eine zentrale asynchrone Ausführung für Lesen/Ungelesen, Stern,
    Wichtigkeit, Label, Archiv, Spam, Löschen, Verschieben und Kopieren.
  - Einzel- und Batch-Aktionen verwenden denselben Worker-/Ergebnisvertrag.
  - Während einer laufenden Aktion werden identische Doppelaktionen verhindert;
    andere Navigation und Fensterinteraktion bleiben möglich.
  - Command-Historie und sichtbares Modell werden erst nach bestätigtem Erfolg
    aktualisiert. Fehler, Konflikt und Abbruch hinterlassen keinen falschen
    Undo-Eintrag und keinen optimistischen Phantomzustand.
  - Nach Erfolg bleiben Ordner, Suche, Sortierung und Nachrichtenauswahl soweit
    möglich erhalten.
- Abnahme:
  - Kein GTK-Signalhandler ruft eine potentiell blockierende Remote-Mutation
    direkt auf.
  - Service-Tests belegen Erfolg, Fehler, Konflikt, Abbruch und Batch-Rollback.
  - View-nahe Tests belegen Aktionssperre und Zustandsübernahme.
  - `cargo check --workspace --locked` und vollständige Testsuite bestehen.
- Linux-Handoff: `LINUX-P0-MAIL-002` in `ROADMAP_LINUX.md`.
- Evidenz: Commit `d6e7dcc`; Einzel-, Batch-, automatische Gelesen-Markierung
  sowie Undo/Redo laufen über Worker und Cancellation-Kontexte. Persistente
  Historie wird außerhalb des GTK-Hauptthreads geladen/geschrieben; Modell und
  History ändern sich erst nach Backendbestätigung. 387/387 Tests bestanden.

### P0-MAIL-001 — MIME-, HTML- und Anhangsdarstellung produktionsfest machen

- Status: `BLOCKED_LINUX` (die portable Viewer- und Render-Strecke ist grün;
  WebKitGTK-Runtime und Churn-Abnahme bleiben Linux-only)
- Quelle: `ROADMAP.md`, Phase 1 „Sicherer WebKitGTK-Viewer“ und „Anhänge“
- Lieferumfang:
  - Ein versioniertes, inhaltsneutrales Testkorpus für `multipart/alternative`,
    verschachtelte Multiparts, fehlerhafte Header, Plaintext, HTML, Inline-CID,
    eingebettete Bilder und reguläre Anhänge.
  - Sichere Mail-CSS-Regeln, Hintergründe und Tabellenlayouts bleiben erhalten;
    JavaScript, externe Stylesheets, Tracking- und Remote-Ressourcen bleiben
    bis zur ausdrücklichen Freigabe blockiert.
  - Ein lesbarer Plaintext-Fallback erscheint bei leerem, defektem oder
    vollständig blockiertem HTML.
  - „Anhang speichern“ meldet Erfolg, Abbruch und Schreibfehler sichtbar und
    verändert keine Nachrichtendaten.
- Abnahme:
  - Parser-/Sanitizer-/Viewer-Komposition ist für alle Korpusfälle getestet.
  - Ein Linux-WebKitGTK-Smoke-Test rendert dieselben Fälle ohne leere Fläche,
    ungefragte Remote-Bilder oder falschen Vollflächenhintergrund.
- Linux-Handoff: `LINUX-P0-MAIL-001` in `ROADMAP_LINUX.md`.
- Evidenz: Commit `d6e7dcc`; zentraler Sanitizer-/Dokumentpfad, explizite
  Loading-/Fehler-/Klartextzustände, CID/Data-Ausnahme und nachrichtenbezogene
  Remote-Freigabe sind portabel getestet. Linux-WebKitGTK-Smoke bleibt offen.

### P0-MAIL-001D — HTML-Rendering vom GTK-Hauptthread entkoppeln

- Status: `BLOCKED_LINUX`
- Quelle: `ROADMAP.md`, aktueller atomarer Mail-Kernblock
- Schreibbereich: `src/views/mail_viewer.rs`, `src/views/mail_page.rs`,
  `src/models/mail_message.rs` und die portablen HTML-/Inline-/Metadaten-
  Services. Native EDS-/Camel-Brücken bleiben außerhalb dieses Pakets.
- Lieferumfang:
  - Eine vollständige Render-Anfrage (Nachrichtenkennung, HTML/Fallback,
    Remote-Policy, Theme- und Schriftwerte) wird vor dem Worker-Lauf
    unveränderlich erfasst.
  - Sanitization, Remote-Bild-Entscheidung, Fallback-Ermittlung und
    Dokumentkomposition laufen außerhalb des GTK-Hauptloops.
  - Jede Antwort trägt die Generation und Anfrageidentität. Nach einem
    Nachrichten- oder Einstellungswechsel verwirft die UI ein veraltetes
    Ergebnis, bevor WebKit, Banner oder Klartext-Fallback aktualisiert werden.
  - Identische aufeinanderfolgende Render-Anfragen werden nicht erneut
    eingeplant. Lade-, Leer-, Fehler- und Fallback-Zustände bleiben explizit.
  - Ein persistenter Latest-Wins-Worker ersetzt wartende veraltete Anfragen;
    CID-Auflösung, Base64 und Sanitizing laufen dort statt im GTK-Kontext.
    Anlagen- und Crypto-Payloads sind referenzgezählt und werden bei Auswahl
    nicht vollständig kopiert.
  - Remote-Content-Freigaben werden einmal pro Viewer geladen. Normale
    Ordner-/Statusfilter sanitizen keinen HTML-Body; echte Body-Suchen nutzen
    einen nach Inhaltsänderung invalidierten Klartextcache.
- Abnahme:
  - Pure Tests belegen Remote-Bild-Erkennung, Fallback-Entscheidung,
    Anfragegleichheit und Stale-Discard bei mindestens zwei Generationen.
  - `git diff --check`
  - `cargo fmt --all -- --check`
  - `cargo check --workspace --locked`
  - `cargo test --workspace --locked --no-fail-fast --quiet`
  - gezielt zusätzlich: `cargo test --workspace --locked mail_viewer -- --nocapture`
  - Ein Linux-WebKitGTK-Lauf mit schnellem Umschalten großer HTML-Nachrichten
    bestätigt, dass nur der zuletzt ausgewählte Inhalt erscheint und der
    Hauptthread bedienbar bleibt; siehe `LINUX-P0-MAIL-003`.
- Statusregel: Vor allen Gates bleibt der Punkt `IN PROGRESS`; ein grüner
  Parser-Test allein darf ihn weder auf `DONE` noch auf Vollparität setzen.
- Evidenz: Pure Tests decken Latest-Wins/Stale, CID-Auflösung, Payload-
  Identität, exakte WebView-URI, Rendercache und Body-Cache-Invalidierung ab.
  Format, Schema, normaler und erzwungener `eds_camel_bridge`-Check sowie
  412/412 Workspace-Tests sind grün; offen bleibt `LINUX-P0-MAIL-003`.

### P0-MAIL-002 — klassischen Evolution-Mailworkflow vervollständigen

- Status: `IN PROGRESS` (Teilpaket `P0-MAIL-002A`; `P0-MAIL-002B` ist portabel
  grün und wartet auf Linux-Abnahme)
- Quelle: `ROADMAP.md`, Phase 1 „Adaptives Layout“, „Mail-Liste“ und
  „Werkzeugleiste“
- Lieferumfang:
  - GTK3-Referenzinventar für Hauptmenüs, Werkzeugleisten, Ordnerbaum,
    Spaltenkopf, Kontextmenüs, Tastaturwege und Detailkopf.
  - Klassische Ansicht als kompakter Standard: Konten/Ordner links,
    einzeilige Nachrichtenliste oben, Vorschau unten.
  - Spaltenwahl, Reihenfolge, Breite, Sortierung, Dichte, Threading,
    Vorschaufenster und Sidebar sind persistent und wirken live.
  - Moderne Ansicht bleibt eine optionale Einstellung und verwendet dieselben
    Daten-, Auswahl- und Aktionspfade.
  - Im Tabellenlauf stehen Anhangs-, Markierungs- und Wichtigkeitsindikatoren
    links vor Absender/Betreff; permanente Auswahlkästchen entfallen, und ein
    Expander erscheint nur bei einem echten Mehrfach-Thread.
  - Konto-Wurzeln bleiben ohne dekoratives Icon, die Ordnerindentation bleibt
    flach, und Badges zeigen ausschließlich nicht-null ungelesene Nachrichten.
    Nach bestätigtem Lesen/Ungelesen- oder Ordner-Refresh wird der Wert reaktiv
    neu berechnet; bei Fehler, Konflikt oder Abbruch bleibt der vorherige
    bestätigte Wert erhalten.
  - Der Routine-Sicherheitsstatus ist standardmäßig unsichtbar. Der UI-Pfad
    startet die Crypto-Prüfung nur nach einem begrenzten Hinweis auf eine
    OpenPGP-/S/MIME-Hülle; gültige, warnende und fehlgeschlagene Prüfungen
    bleiben sichtbar.
  - Die kompakte Zeilenhöhe bleibt mit einer lesbaren 16-Pixel-Mailbasis
    vereinbar; Schrift- und Sicherheitsdetailspräferenzen werden in `002B`
    separat persistent gemacht.
- Abnahme:
  - Jede inventarisierte tägliche Aktion hat einen funktionierenden GTK4-Pfad
    oder einen expliziten Folgepunkt.
  - Keine sichtbaren Backendbegriffe wie EDS, Camel, ECal oder EBook.
  - Bei typischer Desktop-Höhe sind in kompakter Dichte ungefähr 40
    einzeilige Nachrichten sichtbar.
  - Helper-/Layout-Tests belegen Zeilenstatus, Thread-Expander, flache
    Ordnerindentation, Crypto-Preflight, Routinezeilen-Sichtbarkeit und
    Unread-only-Badges.
  - `git diff --check`, `cargo fmt --all -- --check`,
    `cargo check --workspace --locked` und
    `cargo test --workspace --locked --no-fail-fast --quiet` bestehen.
  - Linux-Referenzvergleich für Icons, Dichte und bestätigte Counter besteht;
    siehe `LINUX-P0-MAIL-003`.
- Evidenz: Die portablen Gates einschließlich 422 Workspace-Tests bestehen.
  Der zeilengebundene Sekundärklick selektiert vor
  dem Öffnen des Kontextmenüs exakt seine Nachricht; Label-, Spam- und
  Nicht-Spam-Aktionen sind dort und im Nachricht-Hauptmenü erreichbar.
  Vollständiges Referenzinventar, Icon-/Dichte-Vergleich und reale
  Counter-Abnahme fehlen; deshalb bleibt der Punkt `IN PROGRESS`.

### P0-MAIL-002B — Mailpräferenzen persistent und live wirksam machen

- Status: `BLOCKED_LINUX`
- Abhängigkeit: `P0-MAIL-002A` und die portable Schema-/Dialog-Grundlage.
- Schreibbereich: `src/services/settings_service.rs`,
  `src/dialogs/preferences_dialog.rs`,
  `data/org.gnome.Evolution4.gschema.xml` sowie die betroffenen View-Bindings.
- Lieferumfang:
  - Mail-/Composer-Schriftgröße startet mit einer lesbaren 16-Pixel-Basis,
    bleibt im zulässigen Bereich und wird über GSettings gespeichert,
    zurückgelesen und zurückgesetzt.
  - `mail-show-security-details` ist standardmäßig `false`; `true` zeigt die
    routinehafte Statuszeile optional, ohne echte Krypto-Warnungen oder
    Prüfungsfehler auszublenden.
  - Änderungen an Schrift, Sicherheitsdetails, Spalten, Dichte und Layout
    aktualisieren die bereits geöffnete Mailansicht und überleben einen
    Neustart/Backup-Roundtrip.
- Abnahme:
  - Schema-Kompilierung und Settings-Default-, Roundtrip- und Reset-Tests.
  - `git diff --check`, `cargo fmt --all -- --check`,
    `cargo check --workspace --locked` und
    `cargo test --workspace --locked --no-fail-fast --quiet`.
  - Linux-Abnahme bestätigt die sichtbare Wirkung im laufenden Fenster und
    den Vergleich mit der klassischen Evolution-Anordnung; siehe
    `LINUX-P0-MAIL-003`.
- Statusregel: Ein erfolgreicher GSettings-Roundtrip ohne sichtbare Live-
  Wirkung und Linux-Referenzvergleich ist kein `DONE`-Beleg.
- Evidenz: Schema-Prüfung, Default-/Roundtrip-/Reset-Tests, sofortige
  Sicherheitszeilen-Bindung, Workspace-Check und 396/396 Tests sind grün;
  die sichtbare Linux-Abnahme in `LINUX-P0-MAIL-003` bleibt offen.

### P0-SYNC-001 — Mail-Laden, Paging und Aktualisierung stabilisieren

- Status: `BLOCKED_LINUX`
- Quelle: `ROADMAP.md`, Phase 1 „Asynchrones Laden“ und „Native Camel-Brücke“
- Lieferumfang:
  - Konto-/Ordnerwechsel, Suche, „Mehr laden“, Refresh und Watcher-Ereignisse
    verwenden generationensichere, abbrechbare Requests.
  - UIDs werden dedupliziert; Flag-, Move-, Delete-, Rename- und Folder-Delete-
    Ereignisse aktualisieren Modelle inkrementell.
  - Sortierung, Threading und aktuelle Auswahl bleiben nach einem Reload
    konsistent; ein entferntes Element wählt deterministisch den Nachbarn.
  - Große Ergebnismengen werden begrenzt auf den GTK-Hauptkontext übertragen.
- Abnahme:
  - Deterministische Tests für veraltete Ergebnisse, Duplikate, Paginggrenzen,
    Abbruch und Auswahlwiederherstellung.
  - Linux-Abnahme mit echter Camel-Mailbox und laufenden Änderungen.
- Linux-Handoff: `LINUX-P0-MAIL-001` und `LINUX-P0-RECOVERY-001`.
- Portable Evidenz:
  - Camel und Graph normalisieren Seitengröße/Offset gemeinsam; Graph bewahrt
    den opaken `@odata.nextLink`-Cursor jedes Ordners und lädt jede Folgeseite
    genau über diesen Provider-Cursor statt über positionsbasiertes `$skip`.
  - Backend-Ergebnisse und Change-UIDs werden quellen-/ordnerbezogen
    dedupliziert; leere UIDs werden verworfen und Abbruch wird auch nach
    Provideraufrufen sowie zwischen Parse-/Filter-/Thread-Phasen geprüft.
  - Der Camel-Watcher beendet sich bei Queue-Druck nicht mehr still. Unbekannte
    oder unvollständige Ereignisse fordern einen expliziten Vollabgleich an;
    Rename-Aliase halten nachfolgende UID-Ereignisse adressierbar.
  - Changed-, Folder-Rename- und Folder-Delete-Ereignisse aktualisieren
    Nachrichten, Ordnerbaum, aktive Anfrage und Auswahl inkrementell. Paging,
    Refresh und Sortierung restaurieren die Auswahl per ID/Index; entfernte
    Nachrichten wählen den direkten Nachbarn, leere Modelle leeren den Reader.
  - Nachrichten- und Threadmodelle werden in 32er-Hauptschleifenabschnitten
    gelesen/aufgebaut. Stale Generationen werden vor jeder Modellmutation
    verworfen; abgebrochene Graph-Suchen lassen die UI nicht im Ladezustand.
  - Mail-Wurzeln und übergroße Einzelthreads extrahieren ihre primitiven
    Sortierschlüssel in 32er-GTK-Abschnitten. Ab 128 Einträgen sortiert ein
    Worker ausschließlich diese Schlüssel; sein Indexplan wird anschließend
    wieder stückweise auf die GTK-Modelle angewandt. Auch eine manuelle
    Sortieränderung verwendet damit keinen synchronen Komplettneuaufbau mehr.
  - Der Graph-Transport verwendet abbrechbare Async-HTTP-Futures in einem
    dedizierten Worker. Mail-Laden und Graph-Suche reichen ihren bestehenden
    `Arc<AtomicBool>` bis zu diesem Transport durch; eine laufende langsame
    Antwort wird per Future-Drop beendet und deterministisch als `Cancelled`
    gemeldet.
  - Portable Tests decken Page-Limits, `@odata.nextLink`, Cancellation, stale
    Generationen, UID-/Ergebnisduplikate, Rename-Aliase, Ordnerpfade und
    Nachbarauswahl ab. Normaler und erzwungener `eds_camel_bridge`-Rust-Check
    sind grün.
- Portable Restarbeit: keine für dieses Teilpaket; die verbleibenden Gates
  benötigen echte Provider-/EDS-/Camel-Laufzeit auf Linux.
- Linux-Restarbeit: reale Provider-, Event-Burst-, EDS-Neustart- und
  WebKit/GTK-Reaktionsfähigkeitsabnahme in `LINUX-P0-MAIL-001` und
  `LINUX-P0-RECOVERY-001`; erst nach portabler Restarbeit und Linux-Abnahme
  `DONE`.

### P0-COMP-001 — Antworten, Entwürfe und Versand belastbar machen

- Status: `BLOCKED_LINUX`
- Quelle: `ROADMAP.md`, Phase 2
- Lieferumfang:
  - Neu, Antworten, Allen antworten und Weiterleiten übernehmen Identität,
    Empfänger, Zitat, Anhänge und Format korrekt.
  - Autosave ist atomar und abbrechbar; wiedergeöffnete Entwürfe stellen alle
    sichtbaren Einstellungen wieder her.
  - Versand zeigt Fortschritt und eindeutige Fehler; ein Fehlschlag erhält den
    Entwurf. Erfolgreicher Versand aktualisiert Gesendet/Entwürfe konsistent.
  - Doppelklick oder wiederholtes Aktivieren darf keine Doppelzustellung
    auslösen.
- Abnahme:
  - RFC822-/MIME-Tests einschließlich internationaler Header und Anhänge.
  - Fehler-, Abbruch-, Retry-, Draft-Recovery- und Doppelsende-Tests.
- Linux-Handoff: `LINUX-P0-COMP-001`.
- Evidenz: Commit `d6e7dcc`; Autosave bleibt bis zum Dateiergebnis serialisiert,
  beschädigte Stores werden nicht überschrieben, gleiche Drafts können nicht
  gleichzeitig aus zwei Fenstern gesendet werden und Cleanup löscht im Worker
  nur die bestätigte Revision. Provider-Idempotenz nach unklarem Transportende
  und Drafts-/Sent-Wirkung bleiben Linux-only.

### P0-RECOVERY-001 — Offline/Online, Abbruch und Wiederanlauf absichern

- Status: `BLOCKED_LINUX`
- Quelle: `ROADMAP.md`, Phasen 1, 2 und 7
- Lieferumfang:
  - Offline-Modus stoppt neue Remoteverbindungen und Watcher, erhält aber
    lokale Daten und bereits geladene sichere Zustände.
  - Online-Wechsel lädt Quellen und Modelle ohne Neustart neu.
  - EDS-/D-Bus-/Netzwerkverlust beendet oder invalidiert laufende Arbeit und
    verbindet kontrolliert neu.
  - Beschädigte app-eigene Zustandsdateien werden isoliert gemeldet; originale
    Evolution-/EDS-Daten bleiben unangetastet.
- Abnahme:
  - Portable Tests für Generationen, Cancellation, Backoff und korrupte Stores.
  - Linux-Wiederanlauftest für EDS, GOA, Camel und Desktop-Suspend.
- Linux-Handoff: `LINUX-P0-RECOVERY-001`.
- Evidenz: Offline-Wechsel stoppt Polling, Camel-Watcher und inkrementelle
  Verarbeitung; Online-Wechsel startet Polling und den generationensicheren
  Reload erneut. Die nicht-geheime Source-Discovery-Recovery wird atomar über
  Neustarts gespeichert, verwendet begrenzten exponentiellen Backoff und
  unterscheidet fehlende, gültige, unlesbare und beschädigte Cachedateien.
  Beschädigte app-eigene Dateien werden vor erfolgreichem Neuaufbau
  quarantänisiert. Pure Cache-/Backoff-/Transitions-Tests sind grün; echte
  EDS-/GOA-/Suspend-Abnahme bleibt `LINUX-P0-RECOVERY-001`.

## 6. P1 — tägliche Groupware-Funktionen

### P1-CAL-001 — Kalenderworkflow

- Status: `READY`
- Ziel: Tag, Arbeitswoche, Woche, Monat und Agenda zeigen dieselben Termine;
  Erstellen, Bearbeiten, Löschen, Wiederholung, Teilnehmer, Alarme und
  Einladungsantworten bleiben quellen- und zeitzonenkonsistent.
- Abnahme: portable Kalender-/Recurrence-Tests plus Linux-ECal-Handoff aus
  Abschnitt 6 von `ROADMAP_LINUX.md`.
- Evidenz: `P1-CAL-001A/B` in Commit `d6e7dcc` beziehungsweise `10b0321`:
  gemeinsame RRULE/RDATE/EXDATE-/Mehrtag-/DTEND-Auswertung und 25 gezielte
  Kalender-View-Tests; Teilnehmer-/Einladungs- und Linux-ECal-Abnahme offen.

### P1-CONTACT-001 — Kontakte und Empfänger

- Status: `READY`
- Ziel: vollständiger täglicher Kontakt-CRUD, Suche, Kategorien, Fotos,
  Dubletten und verlustfreier vCard-Roundtrip; Composer-Autocomplete und GAL
  verwenden denselben ausgewählten Kontaktbestand.
- Abnahme: Parser-/Store-/Mutationstests plus Linux-EBook/GAL-Handoff aus
  Abschnitt 7 von `ROADMAP_LINUX.md`.
- Evidenz: `P1-CONTACT-001A/B` in Commit `d6e7dcc` beziehungsweise `10b0321`:
  kompakte quellenneutrale Liste/Detailansicht, Mehrwortsuche, Kategorien und
  sicher begrenzter Dublettenpfad; Linux-EBook/GAL-Abnahme offen.

### P1-TASK-001 — Aufgaben, Memos und Erinnerungen

- Status: `READY`
- Ziel: VTODO/VJOURNAL-CRUD, Status, Priorität, Fälligkeit, Kategorien,
  Sortierung, Suche, Alarme, Snooze und Neustartverhalten entsprechen dem
  Kalender-/Quellenmodell.
- Abnahme: portable Store-/Reminder-Tests plus Linux-ECal-Handoff.
- Evidenz: `P1-TASK-001A` in Commit `d6e7dcc`: Status, Priorität, Fälligkeit
  und Kategorien sind kompakt editierbar; Suche/Filter/Sortierung werden nach
  Mutationen aktualisiert. Memo-/Alarm- und Linux-ECal-Parität bleibt offen.

### P1-SEARCH-001 — Suche, Suchordner und Filter

- Status: `READY`
- Ziel: lokale und native Suche liefern ehrlich gekennzeichnete Ergebnisse;
  Suchordner und Filterregeln sind persistent, geordnet, testbar und melden
  nicht unterstützte Provideroperationen ohne Datenverlust.
- Abnahme: Operator-, Persistenz-, Ausführungs-, Fehler- und Live-Update-Tests;
  native CamelVee-/CamelFilterDriver-Abnahme auf Linux.
- Evidenz: portable Teilimplementierung vorhanden.

## 7. P2/P3 — Vollparität nach stabilen Kernabläufen

Nach Abschluss der P0-/P1-Punkte wird jeder folgende Bereich vor der
Implementierung in gleichartige atomare IDs zerlegt:

- `P2-SEC-*`: OpenPGP, S/MIME, Trust, Widerruf, Schlüssel- und
  Zertifikatsspeicher, Remote-Content-Policy.
- `P2-ENTERPRISE-*`: Graph/EWS-Kompatibilität, OOF, GAL, komplexe Serien,
  Delta- und Offline-Synchronisation.
- `P2-MIGRATION-*`: vollständiger Evolution-Profilimport, Backup, Restore,
  Upgrade und Rollback ohne Geheimnisse oder Überschreiben des Originals.
- `P3-SETTINGS-*`: jede Einstellung der Matrix aus `ROADMAP.md` einzeln mit
  Persistenz, Live-Wirkung, Migration und Backendanwendung.
- `P3-A11Y-*`: Tastatur, Fokus, AT-SPI, High Contrast, RTL, IME und
  Lokalisierung.
- `P3-RELEASE-*`: Flatpak, reproduzierbare/signierte Artefakte, CI,
  Installation, Upgrade, Rollback und Deinstallation.

Die Warteschlange darf nie leer werden, solange eine Pflichtanforderung aus
`ROADMAP.md` oder `ROADMAP_LINUX.md` offen ist.

## 8. Evidenzprotokoll

Für jeden auf `DONE` gesetzten Punkt wird ein Eintrag ergänzt:

```text
ID:
Commit:
Geänderte Pfade:
Automatische Tests:
Manuelle portable Prüfung:
Linux-Abnahme / Verweis:
Negativer Fehlerpfad:
Verbleibende Abweichungen: keine
```

Ein Eintrag mit verbleibender Pflichtabweichung darf nicht `DONE` sein; dafür
wird ein konkreter Folgepunkt angelegt und der Elternpunkt bleibt offen.

### Commit `10b0321` — portable Kernrunde

- `P0-STAB-001A`: 23 Operations-Tests belegen One-shot-Lifecycle,
  Cancellation, Konflikte, bestätigungsbasiertes Undo und expliziten
  Batch-Rollback-Fehler. `P0-STAB-001B` bleibt für die GTK-Worker-Verdrahtung
  offen.
- `P0-MAIL-001A`: byte-sicherer RFC822-/MIME-Parser, verschachtelte
  Multiparts, Transfer-Encoding, gebräuchliche Legacy-Charsets, CID/Anhänge
  und Plaintext-Fallback sind getestet. Linux-Camel-Sonderfälle bleiben offen.
- `P0-MAIL-001B`: eingebettete Newsletter-/Office-CSS bleibt layouttreu;
  aktive Inhalte, externe Stylesheets und verschleierte Ressourcen-URLs sind
  blockiert. Die Viewer-Integration bleibt `P0-MAIL-001C`.
- `P0-COMP-001A`: RFC822, `multipart/alternative`, verschachteltes
  `multipart/mixed`, Unicode-Header, Header-Injection und Anhänge sind durch 14
  Composer-Tests belegt. Dialog-/Draft-/Send-Integration bleibt offen.
- `P1-SEARCH-001A`: 11 Suchtests belegen Evolution-Feldsyntax, Gruppen,
  Negation, Quotes, Escapes, Status und Datumsvergleiche sowie sichere
  Ablehnung nicht unterstützter Syntax.
- `P1-CAL-001A`: 10 Recurrence-Tests belegen `WKST`, `COUNT`, `UNTIL`,
  `EXDATE`, Monats-/Jahresgrenzen und Schaltjahre.
- `P1-CONTACT-001A`: 11 vCard-Tests belegen v3/v4, Folding,
  Mehrfachfelder, Kategorien, Fotos und unbekannte Felder; EBook-Abnahme bleibt
  Linux-only.
- `P2-MIGRATION-001A`: 13 Migrationstests belegen dconf-Werte, sichere
  Fallbacks, redigierte Warnungen sowie den Ausschluss von Geheimnissen.
- Gemeinsame Abnahme: `cargo fmt --all -- --check`,
  `cargo check --workspace --locked` und 359/359 Workspace-Tests bestanden.

### Commit `d6e7dcc` — tägliche GTK4-Workflows

- `P0-STAB-001B`: Mail-Einzel-/Batchaktionen, Auto-Read sowie Undo/Redo laufen
  außerhalb GTK, sind lifecycle-abbrechbar und übernehmen Modell/History erst
  nach bestätigtem Erfolg. Graph-Suche invalidiert ältere Normal-Loads.
- `P0-MAIL-001C`: ein zentral komponiertes, sanitisiertes Maildokument mit
  Klartext-Fallback, expliziten Zuständen und Remote-Freigabe pro Nachricht.
- `P0-COMP-001B`: Worker-basierte MIME-/Transportstrecke, serialisiertes
  Autosave, fail-closed Draft-Store, Doppelsendeschutz und revisionssicheres
  Cleanup; serverseitige Ambiguität bleibt im Linux-Handoff.
- `P1-CAL-001B`, `P1-CONTACT-001B`, `P1-TASK-001A`: gemeinsame
  Kalenderauswertung und kompakte tägliche Kontakt-/Aufgabenworkflows.
- Gemeinsame Abnahme: `cargo fmt --all -- --check`, `git diff --check`,
  `cargo check --workspace --locked` und 387/387 Workspace-Tests bestanden.
