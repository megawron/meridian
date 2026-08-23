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
- `docs/UPSTREAM_GROUPWARE_PARITY_AUDIT.md`,
  `docs/UPSTREAM_MAIL_SHELL_PARITY_AUDIT.md` und
  `docs/UPSTREAM_SETTINGS_PARITY_AUDIT.md` liefern statische Upstream-Belege
  und schneiden daraus portable Pakete sowie ihre Linux-Grenzen.

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
5. Den sauberen offiziellen Evolution-Clone unter
   `/Users/martin/Documents/Projects/evolution-gtk4/evolution-upstream-reference`
   ausschließlich als Referenz für sichtbares Verhalten, Menüstruktur,
   Tastaturbedienung, Dichte und Arbeitsabläufe lesen; dort niemals schreiben.
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
- `PORTABLE_CLOSED`: nur der ausdrücklich benannte portable Handoff eines
  größeren Punktes ist geschlossen; Elternpunkt sowie Linux-, Visual- oder
  Datenmengenabnahmen bleiben offen. Dieser Status ist kein `DONE`- oder
  Vollparitätsbeleg.
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
| 11 | `P0-MAIL-001D` | `P0-MAIL-001` | `src/views/mail_viewer.rs`, `src/views/mail_page.rs`, `src/models/mail_message.rs`, `src/services/inline_content_service.rs` | `P0-MAIL-001A`, `P0-MAIL-001B` | BLOCKED_LINUX | Render-Worker und Body-Suche portabel belegt; WebKitGTK-Churn und native Providerwirkung bleiben Linux-only |
| 11a | `P0-MAIL-001D-SEARCH` | `P0-MAIL-001D` | `src/views/mail_page.rs`, `src/models/mail_message.rs` | `P0-MAIL-001A`, `P0-MAIL-001B` | BLOCKED_LINUX | Worker-Bodytext, vollständige Identität, Crypto-Generation und Filterinvalidierung im aktuellen 618/618-Workspace-Beleg enthalten; Provider-/Linux-Abnahme offen |
| 12 | `P0-MAIL-002A` | `P0-MAIL-002` | `src/views/mail_page.rs`, `src/style.css` | `P0-STAB-001B` | IN PROGRESS | kompakter Evolution-Ordnerbaum ohne Kontoicon, lesbare Tabellenschrift, Icon-Fallbacks und dezente Unread-Zähler |
| 12a | `P0-MAIL-002A-SOURCE` | `P0-MAIL-002A` | `src/views/mail_page.rs`, `src/window.rs`, `src/application.rs`, `src/services/{camel_mail_service,exchange_service}.rs` | `P0-MAIL-002A` | BLOCKED_LINUX | `Ctrl+U`/Menüs, exakter Worker-Abruf, sofortiges wiederverwendbares Lade-/Fehlerfenster sowie Abbruch- und Request-Identitätsgates portabel grün; Byte-/Fenster-Laufzeitabnahme offen |
| 12b | `P0-MAIL-002A-HEADERS` | `P0-MAIL-002A` | `src/views/mail_page.rs`, `src/dialogs/preferences_dialog.rs`, `src/services/{camel_mail_service,exchange_service,mail_store_service,settings_service,backup_service,migration_service,vcard_service}.rs`, `data/org.gnome.Evolution4.gschema.xml` | `P0-MAIL-002A-SOURCE` | BLOCKED_LINUX | vollständige und normale Kopfzeilenansicht, Evolution-`show-headers`-Editor, `Face`-Bild, Graph-Headerübernahme, Persistenz/Migration/Backup und portable Gates grün; Provider-/Dichte-Abnahme offen |
| 12c | `P0-MAIL-WORKER-STORE` | `P0-MAIL-002A` | S0 `src/services/mail_store_service.rs`; S1 `src/views/mail_page.rs`, `src/services/mail_operations_service.rs`; S2 `src/dialogs/preferences_dialog.rs`; S3 `src/services/search_engine.rs`, `src/dialogs/global_search_dialog.rs` | `P0-MAIL-002A` | PORTABLE_CLOSED | S0–S3 portabel geschlossen und reviewt: expliziter Filter-Store, MailPage/Operations-Worker, Preferences-Manual-Filter-Snapshot und Global-Search-Store-Snapshots; der frühere Load/Refresh/Load-more-Handoff ist enthalten; Parent bleibt wegen nativer/performance-/visueller/Großpostfach-, Sidecar-Writer-, Expunge-/Filter-, Graph-/Account-Env- und type-aware-Global-Search-Identity-Resten offen |
| 13 | `P0-MAIL-002B` | `P0-MAIL-002` | `src/views/mail_page.rs`, `src/services/settings_service.rs`, `src/dialogs/preferences_dialog.rs`, `data/org.gnome.Evolution4.gschema.xml` | `P0-MAIL-002A` | BLOCKED_LINUX | Defaults/Roundtrip/Reset und Live-Bindings portabel grün; sichtbare Linux-Abnahme offen |
| 14 | `P0-SYNC-001A` | `P0-SYNC-001` | `src/views/mail_page.rs`, `src/services/camel_mail_service.rs`, `src/services/exchange_service.rs`, `src/models/mail_message.rs` | `P0-STAB-001B` | BLOCKED_LINUX | Portable Paging-, Watcher-, Auswahl-, Sortier- und echter Graph-Abbruchvertrag grün; Camel-Abnahme offen |
| 15 | `P0-COMP-001B` | `P0-COMP-001` | `src/dialogs/compose_dialog.rs`, `src/services/draft_store_service.rs` | `P0-COMP-001A` | DONE | Autosave, Recovery, pro-Draft-Sendelock und revisionssicheres Cleanup integriert |
| 16 | `P0-RECOVERY-001A` | `P0-RECOVERY-001` | `src/services/eds_service.rs`, `src/services/eds_source_cache_service.rs` | Pakete 9–15 | DONE | Offline/Online, persistenter Backoff, atomarer Cache und Neustart portabel getestet |
| 17 | `P1-CAL-001B` | `P1-CAL-001` | `src/views/calendar_page.rs` | `P1-CAL-001A` | DONE | Ansichten verwenden gemeinsame Recurrence-/Zeitzonenregeln |
| 18 | `P1-CONTACT-001B` | `P1-CONTACT-001` | `src/views/contacts_page.rs` | `P1-CONTACT-001A` | IN PROGRESS | `GW-CONTACT-LISTS` ist `BLOCKED_LINUX`; weitere Kontaktaktionen/-suche sowie EBook-/Provider-Reste halten das Elternziel offen |
| 19 | `P1-TASK-001A` | `P1-TASK-001` | `src/views/tasks_page.rs` | keine | IN PROGRESS | Task-CRUD ist belegt und `GW-TASK-FILTERS` ist `BLOCKED_LINUX`; weitere Taskpad-/Editor- und EDS-Reste halten das Elternziel offen |
| 20 | `P1-MAIL-FIND` | `P0-MAIL-002A` | `src/views/{mail_page,mail_viewer}.rs`, `src/window.rs`, `src/application.rs` | `P0-MAIL-001C` | BLOCKED_LINUX | Evolution-Menü/Shortcut, kompakte Suchleiste, Unicode-Fallback, WebKit-FindController, Wrap und stale Generationsgates portabel grün; WebKit-Laufzeitabnahme offen |
| 21 | `MAIL-PRINT-ACTION` | `P0-MAIL-002` | `src/application.rs`, `src/window.rs`, `src/views/{mail_page,mail_viewer}.rs`, `src/services/mail_print_service.rs` | portabler `P0-MAIL-002A`-Auswahlvertrag erfüllt | BLOCKED_LINUX | Liste/Reader, Print/Preview, `Ctrl+P`, sicherer Worker-Snapshot, Generation/Cancel/Error und Reviews portabel belegt; native WebKitGTK-Dialoge/PDF/Visualabnahme offen |
| 22 | `MAIL-TRANSPORT-COMMANDS` | `P0-MAIL-002` | `src/application.rs`, `src/window.rs`, `src/services/mail_transport_service.rs`, `src/services/camel_mail_service.rs`, `src/services/exchange_service.rs`, `src/services/mod.rs` | portabler `P0-COMP-001`-Vertrag erfüllt | BLOCKED_LINUX | Portable Implementierung und unabhängiger Review abgeschlossen: getrennte Actions, F12, State/Progress/Error/Retry/Cancel/Generation, FIFO/Dedupe, Offline-Queue mit Bestätigung erst nach erfolgreichem Backend-Lauf und Transport-Worker-Sicherheit grün; native Camel/EDS/Auth-/Provider-Bestätigung und Live-Fortschrittsabnahme offen |
| 23 | `COMPOSER-ACTION-SURFACE` | `P0-COMP-001` | `src/dialogs/compose_dialog.rs`, `src/services/{draft_store_service,mail_composer_service,settings_service}.rs`, `data/org.gnome.Evolution4.gschema.xml` | `P0-COMP-001B` erfüllt | BLOCKED_LINUX | Actions/Shortcuts, Dirty/Close, atomarer Save-as/Draftwrite, per-message Receipt/Priority/Charset/MIME und Recovery reviewed; Provider-/Desktop-Runtime offen |
| 24 | `GW-TASK-FILTERS` | `P1-TASK-001` | `src/views/tasks_page.rs`, `src/services/settings_service.rs`, `data/org.gnome.Evolution4.gschema.xml` | `P1-TASK-001A` | BLOCKED_LINUX | Upstream-Filter/Advanced/Persistenz im aktuellen portable/macOS-Workspace-Beleg (618/618; keine Linux-Abnahme) enthalten; EDS-Query-/Providersemantik offen |
| 25 | `GW-CONTACT-LISTS` | `P1-CONTACT-001` | `src/services/vcard_service.rs`, `src/services/contact_store_service.rs`, `src/dialogs/contact_dialog.rs`, `src/views/contacts_page.rs` | `P1-CONTACT-001B` | BLOCKED_LINUX | vCard 3/4, Member-Resolution, UI und atomarer Storepfad im aktuellen portable/macOS-Workspace-Beleg (618/618; keine Linux-Abnahme) enthalten; EBook-Capabilities/ACL/Provider offen |
| 26 | `GW-ALARMS` | `P1-CAL-001`, `P1-TASK-001` | `src/services/reminder_service.rs`, `src/dialogs/event_dialog.rs`, `src/views/{tasks,memos}_page.rs` | portable Kalender-/Task-/Memo-Komponenten vorhanden | BLOCKED_LINUX | verlustarmer Multi-VALARM-Editor/Parser und lokaler Snapshot-Kandidatenpfad für Event/Task/Memo reviewed; nur DISPLAY ausführbar, der native EDS-Watcher sowie Provider-/Desktop-Abnahme bleiben offen; das separate `GW-REMINDER-RUNTIME`-Wiring ist portabel abgeschlossen |
| 27 | `GW-MEMO-EDITOR` | `P1-TASK-001` | `src/services/calendar_store_service.rs`, `src/views/memos_page.rs` (Produktionsscope; `src/views/tasks_page.rs` nur test-only Cross-Surface-RFC5545-Test) | `P1-TASK-001A` und portabler `GW-ALARMS`-Vertrag erfüllt | BLOCKED_LINUX | Der Produktionsscope liefert vollständigen portablen VJOURNAL-Draft, fields/attachments/URL/unknown props/VALARM preservation, atomic/cross-process store, Save/Discard/Cancel, stale/confirmed mutation und Export; der separate test-only Test prüft entfaltete ATTACH-Semantik plus 75-Oktett-Folding; native EDS-Capabilities/ACL/Provider-Attachment-Transfer/Desktop-Aktionen offen |
| 28 | `GW-REMINDER-RUNTIME` | `GW-ALARMS` | `src/application.rs`, `src/window.rs`, `src/dialogs/{mod.rs,reminder_dialog.rs}`, `src/services/reminder_service.rs` | portabler `GW-ALARMS`-Vertrag erfüllt | BLOCKED_LINUX | Kanonische Event-/Task-/Memo-Deep-Links und Action-Targets, Source-/UID-Routing, kompaktes wiederverwendbares Reminder-Fenster, Open/10m/1h/Dismiss, exact-once Claim-Leases, Replay-/stale-Generationsgates, atomare persistente Snooze-/Dismiss-/Restart-Semantik, Notification-Vertrag und GTK-freie Worker/MainContext-Grenzen portabel implementiert und unabhängig reviewt; EReminderWatcher/EDS-ClientViews, Provider-Alarme, Sleep/Wakeup, Offline und systemweiter Notification-Daemon bleiben Linux |
| 29 | `GW-CAL-VIEWS-001` | `P1-CAL-001` | `src/views/calendar_page.rs` | `P1-CAL-001A` | BLOCKED_LINUX | Portabler Schnitt geschlossen und reviewt: halb-offene Day/Week/WorkWeek/Month42/Agenda90-Ranges, Navigation, immutable Range-Index, Generation/Cancel/Stale sowie DST/Recurrence; Year/List, Selection, globale Aktionen, native EDS-Range/Monitor/Provider-Wirkung und Visuals bleiben offen |
| 30 | `GW-MEMO-SURFACE-FILTERS` | `P1-TASK-001` | `src/views/memos_page.rs`, `src/services/memo_filter_service.rs`, `src/services/memo_surface_service.rs`, `src/services/settings_service.rs`, `src/services/migration_service.rs`, `src/services/mod.rs`, `data/org.gnome.Evolution4.gschema.xml`, `src/dialogs/preferences_dialog.rs` (geteilter Memo-/Mail-Scope) | `P1-TASK-001A` | BLOCKED_LINUX | Portabler Schnitt geschlossen und reviewt: Source+UID-Multiselect/Action States, Preview/Attachments, strict Search/Category/Source-Filter, Forward-Payload/Open URL/Print/Save/Multi-Export/Copy-Cut-Paste-Delete sowie persistente Keys/Migration; native EDS-ACL/Provider-Anhänge/Browser/Print/iTIP bleiben offen |
| 31 | `SET-01` | `P2-MIGRATION-*`, `P3-SETTINGS-*` | `src/services/{backup_service,migration_service,settings_service}.rs`, `data/org.gnome.Evolution4.gschema.xml` | portable Backup-/Migrationsgrundlage erfüllt | BLOCKED_LINUX | Portable Implementierung und unabhängiger Review abgeschlossen: acht Migrationsparser- und drei Backup-Lücken, Version-4-Backcompat, strikte Validierung, Duplicate-Key-/Redaction-/Restore-/Diagnostics-/Rollback-Tests grün; native GSettings-/Preferences-/EDS-/Camel-/GOA-/Secret-Service-Wirkung offen |

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
- Der Hauptagent ist der Agent-Manager: Er koordiniert die Reihenfolge,
  weist Luna-Max-Subagenten disjunkte Schreibpfade zu, reviewt deren Diffs und
  Berichte, führt die Gates aus und integriert erst danach. Subagenten
  implementieren nur ihren zugewiesenen Pfad; Roadmaps/DOX, Commit und Push
  bleiben beim Hauptagenten.

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

- Status: `BLOCKED_LINUX` (Renderpfad und `P0-MAIL-001D-SEARCH` sind portabel
  belegt; WebKitGTK-/Linux-Churn und native Providerwirkung bleiben offen)
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
  Die CID-Auflösung kodiert nur noch tatsächlich in URI-Attributen
  referenzierte Inline-Bilder und überspringt unreferenzierte Payloads vor der
  Base64-Arbeit.
  Format, strikte Schema-Prüfung, `git diff --check`, normaler und erzwungener
  `eds_camel_bridge`-Cargo-Check sowie ein vollständiger portable/macOS-Lauf
  mit 618/618 Workspace-Tests sind grün belegt; das ist keine Linux-Abnahme.
  Beide unabhängigen Reviews sind abgeschlossen. Offen bleiben `LINUX-P0-MAIL-003` und die
  Provider-Body-Suche.

### P0-MAIL-001D-SEARCH — Worker-vorbereitete Body-Suche

- Status: `BLOCKED_LINUX`
- Quelle: `P0-MAIL-001D` und der aktuelle Mail-Viewer-/Modellvertrag aus
  `src/views/AGENTS.md` und `src/models/AGENTS.md`
- Schreibbereich: `src/views/mail_page.rs`, `src/models/mail_message.rs`;
  keine native EDS-/Camel-Brücke in diesem Paket.
- Lieferumfang:
  - Suchbarer Klartext wird im Worker aus dem HTML-Körper vorbereitet und als
    immutable Cache an die GTK-Modelle übergeben; die Filterstrecke bereitet
    auf Auswahl niemals synchron Bodytext vor.
  - Cache-Schlüssel und Crypto-Ergebnisse verwenden die vollständige
    Nachrichtenidentität plus Generation, damit gleiche IDs in Ordnern oder
    Backends nicht stale Ergebnisse akzeptieren.
  - Camel-first-Merge bei identischen Datensätzen, explizite Invalidation nach
    Body-/Entschlüsselungsersatz und `Different`-Filterbenachrichtigung sind
    Bestandteil des Pakets.
- Abnahme:
  - Pure Tests für worker-vorbereiteten Kleinschreib-/Plaintext, Camel-first-
    Cache, Identitätskollisionen, Crypto-Stale-Gates und Filterinvalidierung.
  - `git diff --check`, `cargo fmt --all -- --check`,
    `cargo check --workspace --locked` und
    `cargo test --workspace --locked --no-fail-fast --quiet`.
  - Ein vollständiger portable/macOS-Workspace-Lauf ist mit 618/618 Tests
    grün belegt; das ist keine Linux-Abnahme. Beide unabhängigen Reviews sind
    abgeschlossen. Das Paket ist wegen der
    folgenden Linux-/Provider-Reste `BLOCKED_LINUX`, nicht `DONE`.
- Linux-/Provider-Rest: serverseitige Body-Suche, Camel-/Graph-Ergebnisgrenzen,
  WebKit-/Reader-Churn und reale EDS-/Providerreaktionen bleiben offen und
  werden über `LINUX-P0-MAIL-001`/`LINUX-P0-MAIL-003` abgenommen.

### P0-MAIL-002 — klassischen Evolution-Mailworkflow vervollständigen

- Status: `IN PROGRESS` (Teilpaket `P0-MAIL-002A`; `P0-MAIL-002B` ist portabel
  grün und wartet auf Linux-Abnahme)
- Geschlossener portabler Handoff: `P0-MAIL-WORKER-STORE` ist mit S0–S3
  `PORTABLE_CLOSED` und unabhängig reviewt. S0 injiziert den Filter-Store
  explizit; S1 umfasst `MailPage` und `MailOperations`; S2 snapshot-t den
  manuellen Preferences-Filterlauf; S3 snapshot-t die Global-Search-Stores
  vor dem Worker und setzt Cancel-/Generation-/Stale-Gates. Exakter
  Produktionsscope:
  `src/services/mail_store_service.rs` (S0),
  `src/views/mail_page.rs` und `src/services/mail_operations_service.rs`
  (S1), `src/dialogs/preferences_dialog.rs` (S2),
  `src/services/search_engine.rs` und
  `src/dialogs/global_search_dialog.rs` (S3). Der frühere
  Load/Refresh/Load-more-Handoff ist enthalten; Worker lesen keine
  GTK/GObject-, GSettings/AppSettings-, Environment- oder mutablen globalen
  Storezustände erneut.
- `P0-MAIL-002A` bleibt offen für native, Performance-, Visual- und
  Großpostfach-Abnahme sowie ausdrücklich Sidecar-Writer-Serialisierung,
  native Expunge-/Filterwirkung, Graph-/Account-Environment und type-aware
  Global-Search-Aktivierungsidentität.
- Quelle: `ROADMAP.md`, Phase 1 „Adaptives Layout“, „Mail-Liste“ und
  „Werkzeugleiste“
- Lieferumfang:
  - Referenzinventar in `docs/MAIL_UI_PARITY_INVENTORY.md` für Hauptmenüs,
    Werkzeugleisten, Ordnerbaum,
    Spaltenkopf, Kontextmenüs, Tastaturwege und Detailkopf.
  - Klassische Ansicht als kompakter Standard: Konten/Ordner links,
    einzeilige Nachrichtenliste oben, Vorschau unten.
  - Spaltenwahl, Reihenfolge, Breite, Sortierung, Dichte, Threading,
    Vorschaufenster und Sidebar sind persistent und wirken live.
  - Moderne Ansicht bleibt eine optionale Einstellung und verwendet dieselben
    Daten-, Auswahl- und Aktionspfade.
  - Im Tabellenlauf stehen Anhangs-, Markierungs- und Wichtigkeitsindikatoren
    links vor Absender/Betreff; permanente Auswahlkästchen entfallen. Kopf und
    Zeile verwenden dieselben festen Zellbreiten, der aktive Sortierpfeil sitzt
    im anklickbaren Spaltenkopf, die erste sichtbare Zeile beginnt grau und
    echte Mehrfach-Threads verwenden eine ausgerichtete Zählzelle statt eines
    zusätzlichen Dropdown-Pfeils.
  - Die klassische Werkzeugleiste beginnt über der linken Ordnerleiste und
    spannt den gesamten Mailbereich. Der Reader behält nur seine kompakten
    Antwortaktionen oberhalb des Betreffs; Routinezustände stehen unten und
    folgen einer persistenten Live-Einstellung.
  - Die Nachverfolgung bleibt vom normalen Stern getrennt und wechselt
    backendbestätigt `None -> FollowUp -> Completed -> None`; der zweite Klick
    zeigt einen grünen Erledigt-Haken. Graph, Camel und lokale Importe bilden
    dieselben drei Zustände providerneutral ab.
  - Konto-Wurzeln bleiben ohne dekoratives Icon, die Ordnerindentation bleibt
    flach, und Badges zeigen ausschließlich nicht-null ungelesene Nachrichten.
    Nach bestätigtem Lesen/Ungelesen- oder Ordner-Refresh wird der Wert reaktiv
    neu berechnet; bei Fehler, Konflikt oder Abbruch bleibt der vorherige
    bestätigte Wert erhalten.
  - `Ctrl+U`, Ansicht- und Kontextmenü öffnen die Nachrichtenquelle unabhängig
    vom Vorschaufenster. Camel und Graph laden die originale RFC822-Quelle in
    einem Worker; ein nicht exakt auflösbarer portabler Datensatz wird sichtbar
    als Rekonstruktion gekennzeichnet. Pro Nachricht erscheint sofort genau ein
    wiederverwendbares Fenster mit Lade-, Quell- und Fehlerzustand; Schließen
    bricht den Abruf ab, und dessen Identität sperrt verspätete Ergebnisse auch
    nach erneutem Öffnen. Die persistente Vollkopfzeilenansicht behält nur den
    ursprünglichen Headerblock statt jeden großen MIME-Body.
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
    Ordnerindentation, feste Zellreihenfolge, erste Zebra-Zeile,
    Crypto-Preflight, Routinezeilen-Sichtbarkeit und Unread-only-Badges.
  - `git diff --check`, `cargo fmt --all -- --check`,
    `cargo check --workspace --locked` und
    `cargo test --workspace --locked --no-fail-fast --quiet` bestehen.
  - Linux-Referenzvergleich für Icons, Dichte und bestätigte Counter steht
    noch aus; siehe `LINUX-P0-MAIL-003`.
- Evidenz: Die portablen Gates einschließlich des einmal vollständigen
  504-Workspace-Testlaufs bestehen;
  beide unabhängigen Reviews der aktuellen angrenzenden Slices sind
  abgeschlossen.
  Der zeilengebundene Sekundärklick selektiert vor
  dem Öffnen des Kontextmenüs exakt seine Nachricht; Label-, Spam- und
  Nicht-Spam-Aktionen sind dort und im Nachricht-Hauptmenü erreichbar.
  Verschieben und Kopieren sind zusätzlich im Kontextmenü, im
  Nachricht-Hauptmenü, in der klassischen Werkzeugleiste und per Tastatur
  erreichbar und funktionieren damit unabhängig von der Vorschaufenster-
  Sichtbarkeit. Dasselbe gilt nun für den EML-Export; auswahlabhängige
  Label-, Verschieben- und Kopieren-Aktionen werden ohne ausgewählte Nachricht
  korrekt deaktiviert. Mailtabelle, Detailkopf und Ordnerbaum verwenden eine
  gemeinsame lesbare 16-Pixel-Basis, während die kompakte Dichte ausschließlich
  über reduzierte Abstände erreicht wird. Nachrichtenquelle und vollständige
  Kopfzeilen sind in den originalen View-/Tastaturwegen eingebunden; der exakte
  Graph-`$value`- und Camel-Abruf läuft außerhalb GTK, während der Header-Fallback
  weder Mailbody noch Anhänge auf dem GTK-Pfad aufbaut. Strukturierte
  Providerzeilen laden ihren exakten Headerblock nur bei eingeblendeter Ansicht;
  Auswahlwechsel, Ausblenden und Dispose brechen alte Abrufe ab, und stale
  Ergebnisse können weder Auswahl noch Kopfzeilen überschreiben. Das separate
  Quellfenster wird unmittelbar im Ladezustand präsentiert und pro Nachricht
  wiederverwendet; Fensterabbruch und Request-Identität verhindern, dass ein
  alter Abruf ein neu geöffnetes Fenster derselben Nachricht überschreibt.
  Der normale Reader verwendet Evolutions geordnete `show-headers`-Defaultliste
  und reagiert live auf Aktivieren, Deaktivieren, Hinzufügen, Entfernen,
  Verschieben und Zurücksetzen im Einstellungseditor. Werte werden als `a(sb)`
  gespeichert, validiert, migriert und gesichert. Graph-Datensätze übernehmen
  `internetMessageHeaders` bereits beim Listenabruf; benutzerdefinierte und
  wiederholte Felder stehen dadurch ohne zweiten Vollquellenabruf bereit.
  „In angezeigter Nachricht suchen“ ist im Bearbeiten-Menü und über
  `Ctrl+Shift+F` erreichbar. Die kompakte Reader-Suchleiste bietet
  Vorwärts-/Rückwärtsnavigation, Trefferstatus, Wrap-around und Escape;
  WebKit verwendet seinen FindController, der portable Fallback arbeitet
  Unicode-zeichengenau. Nachrichten-, Quell- und Rendergeneration verhindern
  Highlights oder Ergebnisse einer vorherigen Auswahl.
  Das ausführbare Referenzinventar liegt in
  `docs/MAIL_UI_PARITY_INVENTORY.md`; Icon-/Dichte-Vergleich und reale
  Counter-Abnahme fehlen weiterhin, deshalb bleibt der Punkt `IN PROGRESS`.

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
  - `mail-show-statusbar` steuert die unteren Routine-Statusbereiche und den
    globalen Status live und persistent; deaktiviert bleiben Fehler weiterhin
    über Toasts/Dialoge erreichbar.
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
  Sicherheitszeilen-Bindung, Headereditor, Backup-/Migrationspfad,
  Workspace-Check und die portable/macOS-Testevidenz (618/618; keine
  Linux-Abnahme) sind grün; die sichtbare Linux-Abnahme in `LINUX-P0-MAIL-003`
  bleibt offen.

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
  nur die bestätigte Revision. `COMPOSER-ACTION-SURFACE` ergänzt reviewed den
  Action-/Shortcut-/Dirty-Vertrag, atomaren EML-Save-as, revisionsgeprüfte
  Draftwrites sowie per-message Receipt/Priority/Charset/MIME/Recovery und ist
  `BLOCKED_LINUX`. Provider-Idempotenz nach unklarem Transportende,
  Desktop-/Crypto-Runtime und Drafts-/Sent-Wirkung bleiben Linux-only.

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

## 5a. Audit-abgeleitete atomare Pakete

Die drei Audits sind statische Quellen-/UI-/Settings-Vergleiche. Sie liefern die
kleinsten nächsten Implementierungszuschnitte, aber keine Laufzeit-, Provider-
oder visuellen Linux-Belege.

### MAIL-PRINT-ACTION

- Status: `BLOCKED_LINUX`; portable Implementierung und unabhängige Reviews
  abgeschlossen.
- Ziel: Eine gemeinsame Print-/Preview-Aktion für aktuelle Auswahl und
  Reader, inklusive stabiler Render-/Jobdaten, `Ctrl+P`, sichtbarem Loading,
  Abbruch und Fehlerpfad.
- Portable Evidenz: Auswahl-/Reader-Snapshot ohne GTK/GObject, deterministisches
  HTML-/Text-Dokument, stabile Header-/Attachment-Reihenfolge, CID-only-
  Einbettung, Remote-/Active-Content-Blockade, Generation, Action-Sensitivität,
  Cancel/Error sowie Menü/Toolbar/`Ctrl+P` sind getestet.
- Linux-Rest: GtkPrint/Portal, PDF-Ziel, HTML/WebKitGTK, Seitenumbrüche und
  visuelle Evolution-Nähe.

### P0-MAIL-WORKER-STORE

- Status: `PORTABLE_CLOSED`; S0–S3 sind portabel implementiert, getestet und
  unabhängig reviewt. Die portable/macOS-Evidenz umfasst `618/618`, Format-,
  Schema-, Diff-, normalen und `eds_camel_bridge`-Check; sie ist keine Linux-
  Abnahme.
- Exakter Produktionsscope: S0 `src/services/mail_store_service.rs`; S1
  `src/views/mail_page.rs` und `src/services/mail_operations_service.rs`; S2
  `src/dialogs/preferences_dialog.rs`; S3 `src/services/search_engine.rs`
  und `src/dialogs/global_search_dialog.rs`. S0 injiziert den Filter-Store,
  S1 deckt MailPage/Operations ab, S2 den Preferences-Manual-Filter-Snapshot
  und S3 die Global-Search-Store-Snapshots vor Workerstart.
- Der frühere `P0-MAIL-002A-WORKER-SETTINGS`-Load/Refresh/Load-more-Handoff
  ist in diesem Paket enthalten. Die Workergrenze verhindert erneutes Lesen
  von GTK/GObject, GSettings/AppSettings, Environment und mutablen globalen
  Stores; Generation/Cancel/Stale bleiben wirksam.
- Parent-Rest: `P0-MAIL-002A` bleibt offen wegen native/performance/visual/
  large-mailbox acceptance sowie ausdrücklich Sidecar-Writer-Serialisierung,
  native Expunge/Filter, Graph/account environment und type-aware Global-
  Search activation identity.

### GW-CAL-VIEWS-001

- Status: `BLOCKED_LINUX`; der portable Schnitt in
  `src/views/calendar_page.rs` ist geschlossen und reviewt.
- Belegt: halb-offene Day/Week/WorkWeek/Month42/Agenda90-Ranges,
  deterministische Navigation, immutable Range-Index, Generation/Cancel/
  Stale-Gates sowie DTEND-exklusive DST-/Recurrence-Regeln. Das ist keine
  Behauptung für Year/List, Selection oder globale Aktionen.
- Linux-Handoff: native EDS-Range-Abfragen müssen dieselben Records für die
  halb-offenen Bereiche liefern; danach ECal-Monitor-/Provider-Refresh, reale
  Zeitzonen/DST/Recurrence, fünf Views und Visuals prüfen. Year/List,
  Selection und globale Aktionen bleiben separate offene Implementierung.

### GW-MEMO-SURFACE-FILTERS

- Status: `BLOCKED_LINUX`; der portable Schnitt ist geschlossen und reviewt.
- Exakter Produktionsscope: `src/views/memos_page.rs`,
  `src/services/memo_filter_service.rs`,
  `src/services/memo_surface_service.rs`, `src/services/settings_service.rs`,
  `src/services/migration_service.rs`, `src/services/mod.rs`,
  `data/org.gnome.Evolution4.gschema.xml` sowie
  `src/dialogs/preferences_dialog.rs` für den geteilten Memo-/Mail-
  Preferences-Scope.
- Belegt: Source+UID-Multiselect/Action States, Preview/Attachments, strict
  Summary/Description/Any/Advanced- und Category/Source-Filter, Forward-
  Payload/Open URL/Print/Save/Multi-Export/Copy-Cut-Paste-Delete und
  persistente Keys/Migration. Native EDS-ACL/Capabilities, Provider-
  Attachment-Transfer, Browser/Print/iTIP und echte Source-/UID-Wirkung
  bleiben Linux-Rest.

### MAIL-TRANSPORT-COMMANDS

- Status: `BLOCKED_LINUX`; portable Implementierung und unabhängiger Review
  abgeschlossen. Der benötigte portable Composer-/Recovery-Vertrag ist
  erfüllt; `GW-REMINDER-RUNTIME` ist als separater portabler Runtime-Schnitt
  ebenfalls abgeschlossen und `BLOCKED_LINUX`. Eine neue portable Priorität
  ist erst nach Audit/Atomisierung des nächsten Pakets zu markieren.
- Ziel: `mail-send-receive`, `mail-receive-all`, `mail-send-all`, `mail-stop`
  und Offline-Zustand als getrennte Action-IDs mit nachvollziehbarem Status,
  Cancellation, Generation und Fehler-/Fortschrittsmodell; `F12` erreicht den
  getrennten Send-/Receive-Weg, ohne `win.refresh-mail`/F5 zu vereinnahmen.
- Portable Abnahme: getrennte Actions, State/Progress/Error/Retry/Cancel/
  Generation, FIFO/Dedupe, Offline-Queue mit Bestätigung erst nach
  erfolgreichem Backend-Lauf und Transport-Worker-Sicherheit mit positiven,
  negativen und stale-Gates; unabhängiger Review und portable/macOS-
  618/618-Workspace-Testevidenz grün, keine Linux-Abnahme. Der portable
  `P0-MAIL-WORKER-STORE`-Handoff ist separat mit S0–S3 geschlossen; seine
  Parent-Reste bleiben im P0-Mail-Abschnitt offen.
- Linux-Rest: Camel/EDS-Transport, Authentifizierung, Provider-Queue und echte
  native Fortschritts-/Fehlerantworten bleiben offen.

### COMPOSER-ACTION-SURFACE

- Status: `BLOCKED_LINUX`; portable Implementierung und unabhängige Reviews
  abgeschlossen.
- Ziel: Upstream-kompatible Composer-Menüs/Toolbars für Send, Draft, Save-as,
  Print, Close/New sowie per-Nachricht Receipt, Priority, Charset,
  Sign/Encrypt und klare Draft-Zustände.
- Portable Evidenz: Actiongraph und lokale Shortcuts, Dirty-/Close-Guard,
  revisionsgeprüfte atomare Draftwrites, atomarer EML-Save-as,
  Draft-Recovery, Doppelsendeschutz, HTML/plain, Attachments sowie
  per-message Receipt/Priority/Charset/MIME-/Security-Optionen sind getestet.
- Linux-Rest: WebKitGTK-/Print-Portal, EDS-Identitäten, Crypto-Backends und
  native Draft-/Sent-/Transportwirkung.

### GW-TASK-FILTERS

- Status: `BLOCKED_LINUX`; portabler Slice im einmal vollständig grünen
  portable/macOS-618/618-Workspace-Lauf (keine Linux-Abnahme) und mit zwei
  abgeschlossenen unabhängigen Reviews.
- Ziel: Upstream-Filter und Advanced-Suche als persistierbarer Query-Vertrag
  für Text, Today/Next-7/30/365, no-due, completed/uncompleted, status,
  starts/due, category und attachments. Unbekannte Importwerte werden als
  begrenzte Teilmenge kenntlich gemacht.
- Portable Evidenz: typed AST, GSettings-Normalisierung/Migration,
  kombinierbare Predikate, live offene Ansicht, Filtertruth-table und DST-
  Grenztests.
- Linux-Rest: EDS-S-Expression/Range-Semantik, Provider-Suchgrenzen,
  Multi-Source-Refresh und upstream-Taskpad-/Editor-Aktionen.

### GW-CONTACT-LISTS

- Status: `BLOCKED_LINUX`; portabler Slice im einmal vollständig grünen
  portable/macOS-618/618-Workspace-Lauf (keine Linux-Abnahme) und mit zwei
  abgeschlossenen unabhängigen Reviews.
- Ziel: Kontaktlisten mit vCard-3-Evolution-Properties und vCard-4-
  `KIND:group`/`MEMBER`, lokalen/externen/verschachtelten Mitgliedern,
  Anzeigenamen, Listenoptionen und sichtbarer Invalidierung.
- Portable Evidenz: Parser-/Serializer-Roundtrips für vCard 3/4, Member-
  Resolution, invalid-member reporting, member-aware GTK-Dialog und atomarer
  Store-/Rollbackpfad.
- Linux-Rest: EBook-Listen-Capabilities, ACLs, serverseitige Member-UIDs,
  Provider-Fehler, Synchronisation und visuelle/Map-/Print-Integration.

### GW-ALARMS

- Status: `BLOCKED_LINUX`; portable Implementierung und unabhängige Reviews
  abgeschlossen.
- Ziel: Ein geordneter, verlustarmer Multi-VALARM-Vertrag für VEVENT, VTODO
  und VJOURNAL mit gemeinsamem Event-/Task-/Memo-Editor und Scheduler.
- Portable Evidenz: relative/absolute Trigger, `RELATED`, `REPEAT`/`DURATION`,
  Attachments, Meldungstext, unbekannte sichere Eigenschaften und Actions
  überstehen Parser-/Serializer-Roundtrips. Nur `DISPLAY` ist ausführbar;
  `AUDIO`, `EMAIL`, `PROCEDURE` und unbekannte Actions bleiben inert. Snapshot-
  Scheduler, Zustell-/Snooze-State und Event-/Task-Fixtures sind deterministisch
  getestet; der lokale Memo-Kandidatenpfad ist im gemeinsamen Snapshot-Vertrag
  enthalten. Die runtime-verdrahtete Notification-/Deep-Link-Aktivierung ist
  als separater portabler Handoff abgeschlossen; der EDS-Watcher bleibt
  Linux-Abnahme.
- Linux-Rest: EDS-ClientView-/Watcher-Snapshot, Source-Änderungen, Factory-
  Neustart, Desktop-Notifications und reale Zeitzonen-/Recurrence-Wirkung.

### GW-REMINDER-RUNTIME

- Status: `BLOCKED_LINUX`; portable Implementierung und unabhängiger Review
  abgeschlossen.
- Portable Abnahme: kanonische Event-/Task-/Memo-Deep-Links und Action-Targets
  mit Source-/UID-Routing, kompaktes wiederverwendbares Reminder-Fenster,
  Open/10m/1h/Dismiss, exact-once Claim-Leases, Replay-/stale-Generationsgates,
  atomare persistente Snooze-/Dismiss-/Restart-Semantik, Notification-Vertrag
  sowie GTK-freie Worker/MainContext-Grenzen sind belegt. Falsche oder stale
  Quellen, fehlende Komponenten, wiederholte Aktivierung und
  Fensterwiederverwendung haben explizite Fehler-/Dedupepfade.
- Linux-Rest: `EReminderWatcher` und EDS-`ClientView` gegen echte Event-/Task- /
  Memo-Quellen, providerseitige Alarme, Sleep/Wakeup, Offline-Wiederaufnahme
  und den systemweiten Notification-Daemon abnehmen; das portable Wiring ist
  kein Linux-Akzeptanzbeleg.

### SET-01 — Backup-/Migrationsvertrag schließen

- Status: `BLOCKED_LINUX`; portable Implementierung und unabhängiger Review
  abgeschlossen. Native Effekte bleiben offen.
- Quelle: `docs/UPSTREAM_SETTINGS_PARITY_AUDIT.md`.
- Ziel: die acht belegten Migrationsparser-Lücken, die drei Backup-Lücken,
  Version 4/Backcompat, den aktuellen Sidebar-Default 240 und den Erhalt eines
  explizit gespeicherten Legacy-JSON-Werts 220 ohne Geheimnis- oder Native-
  EDS-Import schließen.
- Portable Abnahme: strikte Choice-/Bereichs-/String-Validierung,
  Duplicate-Key-Rejection, Redaction auch in Restore/Diagnostics, vollständige
  Vorabvalidierung, atomare Writes/Rollback, neue und ältere Backup-Roundtrips,
  positive/negative Migrationsfixtures und sichtbare Diagnose.
- Linux-Rest: native GSettings-/EDS-/Camel-/GOA-Wirkung und sichtbare
  Restore-/Preferences-Abnahme; ein grüner Key-Roundtrip ist keine Parität.

### GW-MEMO-EDITOR

- Status: `BLOCKED_LINUX`; portable Implementierung und unabhängiger Review
  abgeschlossen.
- Primärer Produktions-Schreibbereich: exakt `src/services/calendar_store_service.rs`
  und `src/views/memos_page.rs`; der separate test-only Cross-Surface-RFC5545-
  Test liegt in `src/views/tasks_page.rs`.
- Ziel/portable Abnahme: vollständiger VJOURNAL-Draft mit UID/DTSTAMP/SUMMARY/
  DESCRIPTION/DTSTART/CLASS/STATUS/URL/CATEGORIES, URI-/BINARY-Anhängen,
  unbekannten Properties/Parametern und geordneten `VALARM`s; atomarer und
  Cross-Process-Store, Save/Discard/Cancel, stale-Generation, bestätigte
  Mutation und Export. Der Cross-Surface-RFC5545-Test vergleicht entfaltete
  semantische ATTACH-Zeilen und prüft 75-Oktett-Folding.
- Linux-Rest: native EDS/VJOURNAL-Capabilities, ACLs, Provider-
  Attachment-Transfer und Desktop-Aktionen (Open URL/Print) bleiben offen.

### Weitere Groupware-/Settings-Lücken aus den Audits

Als weitere disjunkte Folgepakete bleiben:
`GW-CAL-SHELL`, `GW-CAL-ACTIONS`, `GW-EVENT-EDITOR`,
`GW-TASK-SURFACE`, `GW-TASK-EDITOR`,
`GW-CONTACT-ACTIONS`, `GW-CONTACT-SEARCH`,
`GW-SOURCE-SELECTOR`, `GW-SOURCE-MGMT`,
`GW-IMPORT-FRAMEWORK` und `GW-EXPORT-PRINT`. Jedes Paket benötigt einen
eigenen portablen Daten-/UI-Vertrag, negative Tests und einen expliziten
Linux-/EDS-/Desktop-Handoff.

Die Settings-Audit-Slices `MAIL-01..03`, `COMP-01`, `GROUP-01`, `CONTACT-01`,
`ACCOUNT-01`, `NET-01`, `CRYPTO-01` und `PLUGIN-01` werden erst nach atomarem
Zuschnitt und expliziter Abhängigkeitsprüfung auf `READY` gesetzt; das Audit
allein ist kein Implementierungsbeleg.

## 6. P1 — tägliche Groupware-Funktionen

### P1-CAL-001 — Kalenderworkflow

- Status: `IN PROGRESS`
- Ziel: Tag, Arbeitswoche, Woche, Monat und Agenda zeigen dieselben Termine;
  Erstellen, Bearbeiten, Löschen, Wiederholung, Teilnehmer, Alarme und
  Einladungsantworten bleiben quellen- und zeitzonenkonsistent.
- Abnahme: portable Kalender-/Recurrence-Tests plus Linux-ECal-Handoff aus
  Abschnitt 6 von `ROADMAP_LINUX.md`.
- Evidenz: `P1-CAL-001A/B` in Commit `d6e7dcc` beziehungsweise `10b0321`:
  gemeinsame RRULE/RDATE/EXDATE-/Mehrtag-/DTEND-Auswertung und 25 gezielte
  Kalender-View-Tests. `GW-ALARMS` ergänzt den reviewed Multi-VALARM-Editor,
  verlustarmen Parser/Serializer und Event-/Task-/Memo-Scheduler und ist wegen
  des EDS-Watchers `BLOCKED_LINUX`; `GW-MEMO-EDITOR` ergänzt als eigener
  atomarer Schnitt mit exakt diesem Produktionsscope in
  `src/services/calendar_store_service.rs` und `src/views/memos_page.rs` den
  vollständigen portablen VJOURNAL-Draft- /
  Attachment-/URL-/unknown-property-/VALARM-Erhalt sowie atomare
  Save-/Cancel-/Export-Semantik und ist wegen EDS-Capabilities, ACL,
  Provider-Attachment-Transfer und Desktop-Aktionen `BLOCKED_LINUX`.
  Teilnehmer-/Einladungs- und weitere Linux-ECal-Abnahme bleiben offen;
  `GW-REMINDER-RUNTIME` ist als portabler Runtime-Schnitt `BLOCKED_LINUX`;
  EReminderWatcher/EDS-ClientViews und die übrige Linux-Abnahme bleiben offen.

### P1-CONTACT-001 — Kontakte und Empfänger

- Status: `IN PROGRESS`
- Ziel: vollständiger täglicher Kontakt-CRUD, Suche, Kategorien, Fotos,
  Dubletten und verlustfreier vCard-Roundtrip; Composer-Autocomplete und GAL
  verwenden denselben ausgewählten Kontaktbestand.
- Abnahme: Parser-/Store-/Mutationstests plus Linux-EBook/GAL-Handoff aus
  Abschnitt 7 von `ROADMAP_LINUX.md`.
- Evidenz: `P1-CONTACT-001A/B` in Commit `d6e7dcc` beziehungsweise `10b0321`:
  kompakte quellenneutrale Liste/Detailansicht, Mehrwortsuche, Kategorien und
  sicher begrenzter Dublettenpfad. Der aktuelle uncommitted Slice
  `GW-CONTACT-LISTS` ergänzt vCard-3/4-Mitglieder und Member-UI; beide
  unabhängigen Reviews sind abgeschlossen und das Paket ist `BLOCKED_LINUX`.
  Weitere Kontaktflächen sowie Linux-EBook/GAL-/ACL-Abnahme bleiben offen.

### P1-TASK-001 — Aufgaben, Memos und Erinnerungen

- Status: `IN PROGRESS`
- Ziel: VTODO/VJOURNAL-CRUD, Status, Priorität, Fälligkeit, Kategorien,
  Sortierung, Suche, Alarme, Snooze und Neustartverhalten entsprechen dem
  Kalender-/Quellenmodell.
- Abnahme: portable Store-/Reminder-Tests plus Linux-ECal-Handoff.
- Evidenz: `P1-TASK-001A` in Commit `d6e7dcc`: Status, Priorität, Fälligkeit
  und Kategorien sind kompakt editierbar; Suche/Filter/Sortierung werden nach
  Mutationen aktualisiert. Der aktuelle uncommitted Slice `GW-TASK-FILTERS`
  ergänzt Upstream-Filter/Advanced/Persistenz und ist im portable/macOS-
  618/618-Testlauf (keine Linux-Abnahme) sowie zwei abgeschlossenen
  unabhängigen Reviews portabel belegt. Das Paket ist
  `BLOCKED_LINUX`. `GW-ALARMS` ist ebenfalls portabel/reviewt und wegen des
  EDS-Watchers `BLOCKED_LINUX`; `GW-MEMO-EDITOR` ist als eigener
  `BLOCKED_LINUX`-Schnitt abgeschlossen, und `GW-REMINDER-RUNTIME` ist als
  separater portabler Runtime-Schnitt ebenfalls `BLOCKED_LINUX`. Weitere
  Taskpad-Parität bleibt als separate Arbeit offen.

### P1-SEARCH-001 — Suche, Suchordner und Filter

- Status: `BLOCKED_LINUX`
- Ziel: lokale und native Suche liefern ehrlich gekennzeichnete Ergebnisse;
  Suchordner und Filterregeln sind persistent, geordnet, testbar und melden
  nicht unterstützte Provideroperationen ohne Datenverlust.
- Abnahme: Operator-, Persistenz-, Ausführungs-, Fehler- und Live-Update-Tests;
  native CamelVee-/CamelFilterDriver-Abnahme auf Linux.
- Evidenz: Manuelle Filterausführung (`Ctrl+Y` / `Message -> Apply Filters`)
  sowie Mehrfachregel-Filterlauf (`run_all_active_filters`) und vFolder-
  Verwaltung in der Ordnerleiste portabel integriert. Der aktuelle portable
  Arbeitsstand ist im einmal vollständigen portable/macOS-618/618-Workspace-
  Lauf (keine Linux-Abnahme) enthalten; native
  CamelVee/FilterDriver-Laufzeitabnahme auf
  Linux bleibt `LINUX-P1-SEARCH-001`.

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

### Arbeitsstand 2026-08-23 — aktuelle uncommitted Verification-Runde

- Aktuelle portable Slices: `P0-MAIL-WORKER-STORE`, `MAIL-TRANSPORT-COMMANDS`,
  `SET-01`, `GW-CAL-VIEWS-001`, `GW-MEMO-SURFACE-FILTERS`, der eigene
  atomare `GW-MEMO-EDITOR` und `GW-REMINDER-RUNTIME`; sie sind portabel
  implementiert, unabhängig reviewt und wegen nativer/provider-/Desktop-Reste
  `BLOCKED_LINUX`. Die zuvor belegten `MAIL-PRINT-ACTION`,
  `COMPOSER-ACTION-SURFACE` und `GW-ALARMS` bleiben ebenfalls `BLOCKED_LINUX`.
- Portable Evidenz: `git diff --check`, `cargo fmt --all -- --check`, strikte
  Schema-Prüfung, normaler `cargo check --workspace --locked` und
  `RUSTFLAGS='--cfg=eds_camel_bridge' cargo check --workspace --locked` sind
  grün; der finale unsandboxed `cargo test --workspace --locked
  --no-fail-fast --quiet`-Lauf ist mit portable/macOS-Testevidenz von 618/618
  Tests grün; das ist keine Linux-Abnahme. Ein früherer sandboxed Lauf konnte
  18 Loopback-Mockserver nicht binden und ist nicht der
  finale Testbeleg. Die unabhängigen Reviews sind abgeschlossen.
- Keine zusätzliche portable Implementierungspriorität ist derzeit atomar
  `READY`; die geschlossenen Slices sind keine neuen Prioritäten. Vor der
  nächsten Priorisierung muss das nächste verbleibende Auditpaket frisch
  atomar zugeschnitten und mit Schreibbereich, Abhängigkeiten, Positiv-/
  Negativpfad und Linux-Grenze belegt werden.
- Offene Grenzen: Linux-/visuelle-/EDS-/EBook-/ECal-/Camel-/Provider- und
  Desktop-Abnahme bleibt in `ROADMAP_LINUX.md` offen; kein Slice wird daraus
  als `DONE` oder als Vollparität gemeldet.
- Commit/Push: für diese Verification-Runde nicht vorhanden und nicht
  ausgeführt.

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
