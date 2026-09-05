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
| 12c | `P0-MAIL-WORKER-STORE` | `P0-MAIL-002A` | S0 `src/services/mail_store_service.rs`; S1 `src/views/mail_page.rs`, `src/services/mail_operations_service.rs`; S2 `src/dialogs/preferences_dialog.rs`; S3 `src/services/search_engine.rs`, `src/dialogs/global_search_dialog.rs` | `P0-MAIL-002A` | PORTABLE_CLOSED | S0–S3 portabel geschlossen und reviewt: expliziter Filter-Store, MailPage/Operations-Worker, Preferences-Manual-Filter-Snapshot und Global-Search-Store-Snapshots; der frühere Load/Refresh/Load-more-Handoff ist enthalten; Parent bleibt wegen nativer/performance-/visueller/Großpostfach-, Expunge-/Filter- und Graph-/Account-Env-Reste offen |
| 12d | `P0-MAIL-SIDECAR-SERIALIZATION` | `P0-MAIL-002A` | `src/services/mail_metadata_service.rs` | keine | DONE | Mailmetadaten und Remote-Freigaben serialisieren vollständige RMW-Transaktionen pro Sidecar; konkurrierende Writer, Revocation und Lesefehler getestet |
| 12e | `MAIL-DELETE-RECOVERY` | `P0-MAIL-002` | `src/application.rs`, `src/window.rs`, `src/views/mail_page.rs`, `src/services/mail_operations_service.rs` | `P0-STAB-001B` | BLOCKED_LINUX | `Ctrl+Shift+D` stellt nur einen bestätigten Delete-History-Head wieder her; unrelated/busy paths und lokale Trash-Route portabel getestet, native Providerabnahme offen |
| 12f | `FOLDER-CONTEXT-TARGET` | `P0-MAIL-002A` | `src/views/mail_page.rs` | vorhandene Ordneraktionen | BLOCKED_LINUX | Sekundärklick bindet den exakten Folder-Target ohne Navigation; Allowlist, Stale-Clear und ungeladener Mark-All-Pfad getestet, GTK-/HiDPI-Abnahme offen |
| 13 | `P0-MAIL-002B` | `P0-MAIL-002` | `src/views/mail_page.rs`, `src/services/settings_service.rs`, `src/dialogs/preferences_dialog.rs`, `data/org.gnome.Evolution4.gschema.xml` | `P0-MAIL-002A` | BLOCKED_LINUX | Defaults/Roundtrip/Reset und Live-Bindings portabel grün; sichtbare Linux-Abnahme offen |
| 14 | `P0-SYNC-001A` | `P0-SYNC-001` | `src/views/mail_page.rs`, `src/services/camel_mail_service.rs`, `src/services/exchange_service.rs`, `src/models/mail_message.rs` | `P0-STAB-001B` | BLOCKED_LINUX | Portable Paging-, Watcher-, Auswahl-, Sortier- und echter Graph-Abbruchvertrag grün; Camel-Abnahme offen |
| 15 | `P0-COMP-001B` | `P0-COMP-001` | `src/dialogs/compose_dialog.rs`, `src/services/draft_store_service.rs` | `P0-COMP-001A` | DONE | Autosave, Recovery, pro-Draft-Sendelock und revisionssicheres Cleanup integriert |
| 16 | `P0-RECOVERY-001A` | `P0-RECOVERY-001` | `src/services/eds_service.rs`, `src/services/eds_source_cache_service.rs` | Pakete 9–15 | DONE | Offline/Online, persistenter Backoff, atomarer Cache und Neustart portabel getestet |
| 17 | `P1-CAL-001B` | `P1-CAL-001` | `src/views/calendar_page.rs` | `P1-CAL-001A` | DONE | Ansichten verwenden gemeinsame Recurrence-/Zeitzonenregeln |
| 18 | `P1-CONTACT-001B` | `P1-CONTACT-001` | `src/views/contacts_page.rs` | `P1-CONTACT-001A` | IN PROGRESS | `GW-CONTACT-LISTS`, `GW-CONTACT-ACTIONS-*` und `GW-CONTACT-SEARCH` sind `BLOCKED_LINUX`; weitere EBook-/Provider-Reste halten das Elternziel offen |
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
| 29 | `GW-CAL-VIEWS-001` | `P1-CAL-001` | `src/views/calendar_page.rs` | `P1-CAL-001A` | BLOCKED_LINUX | Portabler Schnitt geschlossen und reviewt: halb-offene Day/Week/WorkWeek/Month42/Agenda90/Year/List-Ranges, Navigation, immutable Range-Index, Generation/Cancel/Stale sowie DST/Recurrence; Selection, globale Aktionen, native EDS-Range/Monitor/Provider-Wirkung und Visuals bleiben offen |
| 30 | `GW-MEMO-SURFACE-FILTERS` | `P1-TASK-001` | `src/views/memos_page.rs`, `src/services/memo_filter_service.rs`, `src/services/memo_surface_service.rs`, `src/services/settings_service.rs`, `src/services/migration_service.rs`, `src/services/mod.rs`, `data/org.gnome.Evolution4.gschema.xml`, `src/dialogs/preferences_dialog.rs` (geteilter Memo-/Mail-Scope) | `P1-TASK-001A` | BLOCKED_LINUX | Portabler Schnitt geschlossen und reviewt: Source+UID-Multiselect/Action States, Preview/Attachments, strict Search/Category/Source-Filter, Forward-Payload/Open URL/Print/Save/Multi-Export/Copy-Cut-Paste-Delete sowie persistente Keys/Migration; native EDS-ACL/Provider-Anhänge/Browser/Print/iTIP bleiben offen |
| 31 | `SET-01` | `P2-MIGRATION-*`, `P3-SETTINGS-*` | `src/services/{backup_service,migration_service,settings_service}.rs`, `data/org.gnome.Evolution4.gschema.xml` | portable Backup-/Migrationsgrundlage erfüllt | BLOCKED_LINUX | Portable Implementierung und unabhängiger Review abgeschlossen: acht Migrationsparser- und drei Backup-Lücken, Version-4-Backcompat, strikte Validierung, Duplicate-Key-/Redaction-/Restore-/Diagnostics-/Rollback-Tests grün; native GSettings-/Preferences-/EDS-/Camel-/GOA-/Secret-Service-Wirkung offen |
| 32 | `MAIL-ACCELERATOR-CONTRACT` | `P0-MAIL-002` | `src/application.rs`, `src/window.rs`, `src/views/mail_page.rs`, `src/dialogs/shortcuts_dialog.rs` | Mail-Actions vorhanden | BLOCKED_LINUX | konfliktfreie Action-Tabelle, Upstream-Next/Previous/Unread-, Preview-, Thread-, Print- und Find-Bindings, Menü-/Help-Overlay-Parität sowie Empty/Boundary-Tests portabel grün; GTK/IME/Fokus-Abnahme offen |
| 33 | `GW-CAL-ACTIONS-TRANSFER-DELETE` | `P1-CAL-001` | `src/views/calendar_page.rs` | portabler Kalender-CRUD-Vertrag | BLOCKED_LINUX | Copy/Move nach Lokal ist create-before-remove und lädt bei Teilfehlern neu; Serien-Löschumfang fragt explizit und lehnt nicht verlustfrei unterstützte Scopes ohne Mutation ab; beliebige EDS-Ziele und Occurrence-Mutationen offen |
| 34 | `GW-CONTACT-ACTIONS-BATCH-EXPORT` | `P1-CONTACT-001` | `src/views/contacts_page.rs` | `P1-CONTACT-001B` | BLOCKED_LINUX | 0/1/n-Auswahl, Batch-Mail, action sensitivity und atomarer gemeinsamer vCard-Export portabel getestet; Batch-Delete/Transfer/Print und EBook-/Desktop-Abnahme offen |
| 35 | `GW-TASK-EDITOR-LIFECYCLE` | `P1-TASK-001` | `src/views/tasks_page.rs` | portabler VTODO-Editor | BLOCKED_LINUX | Unabhängiger Erhalt von STATUS/PERCENT-COMPLETE/COMPLETED bei Fremdfeldänderungen einschließlich fehlender/unsupported Werte sowie explizite gekoppelte Normalisierung getestet; native EDS-Capabilities/Providerwirkung offen |
| 36 | `MAIL-FOLLOW-UP-DUE` | `P0-MAIL-002` | `src/services/{mail_metadata_service,mail_store_service,mail_operations_service,camel_mail_service,exchange_service,search_engine}.rs`, `src/models/mail_message.rs`, `src/views/mail_page.rs`, `src/window.rs` | Follow-up-Dreistatus vorhanden | BLOCKED_LINUX | Text/Due/Completed-Metadaten, v7-enum Migration, expliziter Editor/Menüaction, exakte Undo-Inverse, Camel-Parse/Write und Graph-Fail-closed sind portabel getestet; native FFI-/Visualabnahme offen |
| 37 | `GW-TASK-ACTIONS-BATCH` | `P1-TASK-001` | `src/views/tasks_page.rs` | portabler VTODO-Listen-/Exportvertrag | BLOCKED_LINUX | Echte 0/1/n-Auswahl, Source+UID-Identität, sichtbarkeitsbereinigte Action-Sensitivity und vorvalidierter atomarer gemeinsamer VCALENDAR-Export sind getestet; weitere Batchmutationen, native iTIP-/URL-/Print- und EDS-/Desktop-Abnahme offen |
| 38 | `GW-TASK-ACTIONS-FORWARD-URL` | `P1-TASK-001` | `src/views/tasks_page.rs` | `GW-TASK-ACTIONS-BATCH` | BLOCKED_LINUX | Exakte Single-Selection, UID-validierter alarmfreier METHOD:PUBLISH-Payload und gehärtete HTTP(S)-URL-Aktion sind portabel getestet; echte iTIP-Zustellung und Desktop-/EDS-Abnahme offen |
| 39 | `GW-TASK-ACTIONS-PRINT` | `P1-TASK-001` | `src/views/tasks_page.rs` | `GW-TASK-ACTIONS-BATCH` | BLOCKED_LINUX | Exakte Single-Selection und deterministisches, escaped, remote-freies HTML-/Text-Druckdokument mit sichtbarem Preparing/Result/Error-Handoff sind getestet; nativer GTK-Druckdialog/Portal und Jobabschluss offen |
| 40 | `GW-TASK-ACTIONS-LIFECYCLE-BATCH` | `P1-TASK-001` | `src/views/tasks_page.rs` | `GW-TASK-ACTIONS-BATCH`, `GW-TASK-EDITOR-LIFECYCLE` | BLOCKED_LINUX | Upstream-Sensitivity, all-writable exact preflight, Complete/Incomplete-Transformation, sequentielle ECal-Ergebnisse, partial failure und authoritative success fallback sind portabel getestet; externe EDS-Writability/Monitor-/Providerabnahme offen |
| 41 | `GW-TASK-ACTIONS-DELETE-BATCH` | `P1-TASK-001` | `src/views/tasks_page.rs` | `GW-TASK-ACTIONS-BATCH` | BLOCKED_LINUX | Exakte all-writable Auswahl, eine named/count Confirmation, Re-Preflight, sequentielles Stop-on-first-error-Delete und bestätigter Prefix/Failure/Unattempted-Reducer sind getestet; RECURRENCE-ID MOD_THIS, Single-Assign-iTIP und Providerabnahme offen |
| 42 | `GW-TASK-ACTIONS-CLIPBOARD` | `P1-TASK-001` | `src/views/tasks_page.rs` | `GW-TASK-ACTIONS-BATCH`, `GW-TASK-ACTIONS-DELETE-BATCH` | BLOCKED_LINUX | Copy/Cut/Paste/Select All, Tasklisten-Fokusshortcuts, Default-Source-Revalidation, Fresh-UID-Paste und bestätigter Prefix/Failure/Unattempted-Reducer sind getestet; VTIMEZONE/TZID-Provisionierung und externe Providerabnahme offen |
| 43 | `GW-TASK-ACTIONS-BULK-EDIT` | `P1-TASK-001` | `src/views/tasks_page.rs` | `GW-TASK-ACTIONS-BATCH`, `GW-TASK-EDITOR-LIFECYCLE` | BLOCKED_LINUX | Upstream-sensitive Ctrl+B-Aktion, opt-in Feld-/Kategorieänderungen, Lifecycle-/Datums-Kopplung, sequential stop-first-error und authoritative Prefix/Recovery sind getestet; VTIMEZONE und externe Providerabnahme offen |
| 44 | `GW-TASK-ACTIONS-FIND-PREVIEW` | `P1-TASK-001` | `src/views/tasks_page.rs`, `src/services/{settings_service,migration_service}.rs`, `data/org.gnome.Evolution4.gschema.xml` | `GW-TASK-ACTIONS-BATCH` | BLOCKED_LINUX | Exakte Single-Selection-Vorschau, persistente Classic/Vertical-Pane samt getrennten Divider-Positionen, Ctrl+M, Ctrl+Shift+F, Unicode-Wrap-Suche, sichere VTODO-/Attachment-Metadaten und Upstream-Settingsmigration sind getestet; HTML-/Attachment-Aktivierung und Linux-Visuals bleiben offen |
| 45 | `GW-TASK-ACTIONS-PURGE` | `P1-TASK-001` | `src/views/tasks_page.rs` | `GW-TASK-ACTIONS-DELETE-BATCH` | BLOCKED_LINUX | Selection-unabhängiges Ctrl+E, destructive Re-Preflight aller erledigten Tasks in writable Sources, Read-only-Skip, sequential confirmed Prefix und authoritative Channel-Recovery sind getestet; RECURRENCE-ID/MOD_THIS und Providerabnahme offen |
| 46 | `GW-TASK-ACTIONS-LIST-PRINT` | `P1-TASK-001` | `src/views/tasks_page.rs` | `GW-TASK-ACTIONS-BATCH`, `GW-TASK-ACTIONS-PRINT` | BLOCKED_LINUX | Selection-unabhängige sichtbare Listenreihenfolge, exakte Source+UID-Revalidation, escaped HTML/Text, getrennte Print-/Preview-Preparation und Tasklisten-Fokus-Ctrl+P sind getestet; nativer GTK-Print/Preview-Dialog und Linux-Visuals offen |
| 47 | `GW-TASK-LIST-REFRESH` | `P1-TASK-001` | `src/views/tasks_page.rs`, `src/services/ecal_service.rs`, `native/eds_ecal_bridge.{h,c}` | exakte primäre Aufgabenlistenquelle | BLOCKED_LINUX | Auswahlgebundene, mutationsgesperrte Aktualisierung prüft exakte Source-UID und TASK_LIST-Typ, EClient-Refresh-Support und bestätigt erst nach Provider-Refresh plus autoritativem Reload; Generation/Quellenwechsel verwerfen stale Ergebnisse, Unsupported und Backendfehler bleiben sichtbar; EDS-Provider-/Auth-/Offline-Abnahme offen |
| 48 | `GW-TASK-SOURCE-CAPABILITIES` | `P1-TASK-001` | `src/views/tasks_page.rs`, `src/services/ecal_service.rs`, `native/eds_ecal_bridge.{h,c}` | native ECal-Brücke | BLOCKED_LINUX | Exakte enabled TASK_LIST-Enumeration ersetzt generische Calendar-/GOA-Quellen; ECalClient connected/available, readonly/writable und refresh-supported treiben Listen-, Editor-, Row- und Action-Sensitivity, Partial-Connection-Fehler bleiben sichtbar und unavailable Quellen erhalten ihre Identität; Registry-/Provider-/ACL- und Linux-Visualabnahme offen |
| 49 | `GW-MEMO-SOURCE-CAPABILITIES` | `P1-TASK-001` | `src/views/memos_page.rs` | `GW-TASK-SOURCE-CAPABILITIES` (wiederverwendbare ECal-Discovery) | BLOCKED_LINUX | Exakte enabled MEMO_LIST-UIDs und connected/available/readonly-writable ersetzen generische Calendar-Quellen in Hauptliste und Editor; unbekannte, offline und read-only Quellen mutieren fail-closed, unavailable Filter-/Edit-Identitäten bleiben sichtbar; reale EDS-/ACL-/Provider- und Linux-Visualabnahme offen |
| 50 | `GW-TASK-LIST-MANAGEMENT` | `P1-TASK-001` | `src/views/tasks_page.rs`, `src/services/ecal_service.rs`, `native/eds_ecal_bridge.{h,c}` | `GW-TASK-SOURCE-CAPABILITIES` | BLOCKED_LINUX | Exakte Primary-Source-Metadaten treiben Copy/Rename/Delete; Rename/Properties verwenden `ESource:writable` getrennt von der Client-Writability für Taskmutationen, Delete wählt remote-delete nur bei Capability, Copy überträgt alle VTODOs samt TZIDs und überschreibt gleiche UIDs mit ehrlichem Partial-Prefix. New erstellt eine lokale TASK_LIST unter `local-stub`; lokale Properties bearbeiten Name/Farbe/Default/ICS-Datei/Writability/E-Mail. Nicht-lokale provider-spezifische New/Properties-Konfigurationsdialoge und reale EDS-Laufzeitabnahme bleiben offen. |
| 51 | `GW-TASK-ACTIONS-ASSIGN` | `P1-TASK-001` | `src/views/tasks_page.rs`, `src/services/ecal_service.rs`, `native/eds_ecal_bridge.{h,c}` | `GW-TASK-SOURCE-CAPABILITIES`, `GW-TASK-ACTIONS-BATCH` | BLOCKED_LINUX | Exakte Single-Selection plus connected/client-writable und beide ECal-Assignment-Capabilities treiben Assign. Organizer/Attendees werden validiert und ohne Verlust fremder/nested Properties ersetzt, über exact Source+UID gespeichert und autoritativ neu geladen; METHOD:REQUEST wird bereitgestellt. Echter iTIP-Versand, Delegate-Ketten und Provider-/Linux-Abnahme bleiben offen. |
| 52 | `COMPOSER-PRINT-LIFECYCLE` | `P0-COMP-001` | `src/dialogs/compose_dialog.rs`, `src/views/{mod.rs,mail_viewer.rs}`, `src/services/mail_print_service.rs` | `MAIL-PRINT-ACTION`, `COMPOSER-ACTION-SURFACE` | BLOCKED_LINUX | Composer Print/Preview erfassen Header, HTML/plain und Anlagenmetadaten unveränderlich auf GTK, bereiten das remote-freie Dokument abbrechbar im Worker vor, sperren kollidierende Composer-Aktionen und verwerfen stale/teardown callbacks. Preview verwendet den isolierten Viewer, Print den nativen WebKit-Dialog; Linux-/Portal-/PDF-Visualabnahme offen. |
| 53 | `MAIL-CAMEL-FOLDER-MUTATIONS` | `P0-MAIL-002` | `native/eds_camel_bridge.{h,c}`, `src/services/camel_mail_service.rs`, `src/views/mail_page.rs`, `src/window.rs` | `FOLDER-CONTEXT-TARGET` | BLOCKED_LINUX | Exakte native Camel-Folder-Capabilities treiben CreateChild/Rename/Delete/Refresh. UI-Worker sind teardown-abbrechbar, Provider-Ergebnis wird durch frische Hierarchie verifiziert und aktive Rename/Delete-Ziele werden erst danach reconciled. Graph, Top-Level-Create, geschützte/unsafe Targets sowie nicht-atomare Copy/Move bleiben fail-closed; reale Providerabnahme offen. |
| 54 | `LOCAL-ACCOUNT-ORDER-DEFAULT` | `P0-COMP-001` | `src/services/account_config_service.rs`, `src/dialogs/{preferences,compose}_dialog.rs` | app-eigene lokale Profile | DONE | Persistente Up/Down-Reihenfolge, sichtbarer Default, Default-Reparatur nach Disable/Delete und Composer-Fallback sind getestet. Externe EDS-Defaults bleiben unberührt; gecachte EDS-Zeilen sind factual read-only. EDS-/GOA-Service-Mutation bleibt native Folgearbeit. |
| 55 | `MAIL-LIST-METADATA-COLUMNS` | `P0-MAIL-001` | Schema/Settings/Backup, `src/models/mail_message.rs`, `src/services/{mail_store,exchange}_service.rs`, `src/dialogs/preferences_dialog.rs`, `src/views/mail_page.rs` | Nachrichtenliste mit Follow-up- und Backend-Metadaten | DONE | Optionale Size-/Due-/Location-Spalten sind live schaltbar und sortierbar. Exakte RFC822-/Graph-Größe, Follow-up-Due und Konto/Vollordner-Location fließen durch Modell, Migration/Reset und Backup. Der bestehende Follow-up-Sortselector-Versatz ist behoben; Reihenfolge und Ansichtsprofile schließt Paket 63. |
| 56 | `MAIL-PREVIEW-TOOLBAR-COMMON` | `P0-MAIL-001` | `src/views/mail_page.rs` | verdrahtete Reader- und Navigationsaktionen | DONE | Reader Common-Actions Delete/Junk/Not-Junk/Archive und Previous/Next werden tatsächlich angezeigt; Classic Toolbar enthält ebenfalls Previous/Next. Zustandsbindung nutzt die vorhandenen Window-Actions bzw. echten Reader-Operationen. Overflow-/Theme-Abnahme bleibt visuell. |
| 57 | `READER-REPLY-FORWARD-VARIANTS` | `P0-MAIL-001` | `src/dialogs/compose_dialog.rs`, `src/views/mail_page.rs`, `src/window.rs` | bestehende Reply-/Forward-Composerpfade | DONE | `List-Post`-gebundenes Listen-Reply sowie echte Inline- und `>`-zitierte Forward-Modi sind in Message-/Kontextmenü und zentraler Action-Sensitivity verdrahtet. Ohne echte Nachricht sind Reader-Aktionen deaktiviert; NO/malformed/injection List-Post bleibt fail-closed. Alternative-Reply-Dialog und Attached-Forward bleiben getrennte Folgepakete. |
| 58 | `READER-ALTERNATIVE-REPLY` | `P0-MAIL-001` | `src/dialogs/compose_dialog.rs`, `src/views/mail_page.rs`, `src/window.rs` | `READER-REPLY-FORWARD-VARIANTS` | DONE | Alternative Reply bietet Sender, validierte Mailingliste oder All sowie Default-, Outlook-Inline-, `>`-Zitat- und No-Quote-Stil. Die Auswahl gilt nur für den neuen Composer; der upstream Attachment-Stil ist sichtbar, aber bis zu einem draft-sicheren RFC822-Anhangspeicher explizit deaktiviert. |
| 59 | `READER-ATTACHED-FORWARD` | `P0-MAIL-001` | `src/services/{draft_store,mail_composer}_service.rs`, `src/dialogs/compose_dialog.rs`, `src/views/mail_page.rs`, `src/window.rs` | exakter RFC822-Exportpfad | DONE | Attached Forward lädt vorhandene lokale/native/Graph-RFC822-Bytes asynchron und fällt nur auf den bestehenden verlustarmen Exportbuilder zurück. Der `message/rfc822`-Payload liegt Base64-kodiert im DraftAttachment, überlebt Autosave/Reopen und wird vor MIME-Bau strikt auf Encoding und Größe geprüft; Attached Reply bleibt offen. |
| 60 | `READER-ATTACHED-REPLY` | `P0-MAIL-001` | `src/dialogs/compose_dialog.rs`, `src/views/mail_page.rs` | `READER-ALTERNATIVE-REPLY`, `READER-ATTACHED-FORWARD` | DONE | Der Alternative-Reply-Attachment-Stil ist für Sender, validierte Liste und All aktiv. Er teilt den asynchronen exact-RFC822-/Fallback-Loader und das draft-sichere Embedded-Attachment mit Attached Forward; der Composer setzt Reply-Empfänger/Cc/Betreff und hängt das Original ohne temporäre Datei an. |
| 61 | `READER-DAILY-ACTIONS` | `P0-MAIL-001` | `src/dialogs/{compose,contact}_dialog.rs`, `src/services/{draft_store,mail_composer}_service.rs`, `src/views/mail_page.rs`, `src/window.rs` | geladene exakte Nachrichtenmetadaten | DONE | Copy Message Link kopiert einen validierten `mid:`-URI; Containing Folder wechselt aus Aggregat/Suchordner zum exakten lokalen oder geladenen Providerordner und selektiert dieselbe Nachricht; Add Sender öffnet den echten vorbefüllten EBook-/Lokal-Kontakteditor; Edit as New übernimmt Empfänger, Betreff, Body und eingebettete Anhänge. MIME Content-ID bleibt dabei draft- und versandsicher erhalten, sodass `cid:`-Inlinebilder nicht still beschädigt werden. |
| 62 | `GW-CAL-ACTIONS-PURGE-LOCAL` | `P1-CAL-001` | `src/services/calendar_store_service.rs`, `src/dialogs/calendar_purge_dialog.rs`, `src/views/calendar_page.rs`, `src/{application,window}.rs` | app-eigener lokaler Kalender | BLOCKED_LINUX | `Ctrl+E` ist kalendergebunden; 60-Tage-Default/0..1000-Cutoff, Worker/Cancel/Doppelstart, einmaliger atomarer Dateirewrite, autoritativer Reload und sichtbare Resultate sind portabel getestet. Serien/RECURRENCE-ID/Duplicate-UID/DURATION/ungültige Grenzen bleiben fail-closed; aktive ECal-Clients und Provider-Purge bleiben Linux. |
| 63 | `MAIL-LIST-VIEW-CONTRACT` | `P0-MAIL-002A` | `data/org.gnome.Evolution4.gschema.xml`, `src/services/{settings,backup}_service.rs`, `src/dialogs/preferences_dialog.rs`, `src/views/mail_page.rs` | bestehende Mail-Spalten und Live-Settings | BLOCKED_LINUX | Evolutions Messages-/Wide-Profile, vollständige eindeutige benutzerdefinierte Reihenfolge, gemeinsame stabile Header-/Zeilen-IDs, Live-Neuaufbau ohne Provider-Reload sowie Sender-/Datumsbreite in Migration und Backup sind portabel getestet. Drag-and-drop, HiDPI-/Theme-/A11y- und Großpostfach-Abnahme bleiben Linux. |
| 64 | `MENU-ACTION-MAP` | `P0-MAIL-002A` | `src/action_contract.rs`, `src/{application,window}.rs`, `src/dialogs/shortcuts_dialog.rs` | vorhandene Shell-/Reader-/Folder-Actions und Accelerator-Tabelle | BLOCKED_LINUX | Ein eindeutiger Vertrag treibt File/Edit/View/Message/Folder/Tools/Help, Transport und Shortcut-Hilfe samt Zustandsvoraussetzungen. Menü-/Shortcut-IDs, Registrierungen, Platzierungen, Accelerator-Mirror und Nullauswahl sind portabel getestet; Popup-Vollabgleich, GTK/IME/Fokus, Theme/A11y und reale Providerzustände bleiben Linux. |
| 65 | `MAIL-SEARCH-CONTRACT` | `P1-SEARCH-001` | `src/services/{search_engine,search_folder}_service.rs`, `src/dialogs/advanced_search_dialog.rs`, `src/views/mail_page.rs` | `P1-SEARCH-001A`, `P0-MAIL-WORKER-STORE` | BLOCKED_LINUX | Kanonische Syntaxvalidierung für neue Saved Searches, fail-closed Unknown-State-Negation, exakte `CurrentFolder`/`CurrentFolderAndSubfolders`/`CurrentAccount`/`AllAccounts`-Scopes sowie geschützte Saved-Search-Mutationen (corrupt/unsupported/duplicate, eindeutige temporäre Dateien, atomare Writes und process-lokale RMW-Serialisierung) sind portabel belegt. Saved-Search-v2 persistiert Feld, Scope, exakte Konto-/Ordneridentität und versionierten AST, migriert v1 deterministisch und verwendet für Auswahl und Zähler denselben fail-closed Compiler; relative Daten bleiben dynamisch. Die sieben sichtbaren Upstream-Schnellmodi `SubjectOrAddresses`, `Subject`, `Sender`, `Recipients` (To/Cc, ohne Bcc), `Message`, `Body` und `FreeForm` verwenden den kanonischen AST/Matcher; MailPage verwendet ausschließlich den kanonischen AST. Der mail-sensitive Advanced-Search-Dialog erzeugt portable Text-/Status-/Datumsregeln mit UND/ODER sowie Regel-/Gruppen-Negation, validiert sie vor dem Anwenden und erhält Scope und exaktes Ordnerziel. Graph senkt nur eine strict-safe positive Konjunktion ab; OR/NOT und local-only-Prädikate werden als Ganzes nicht remote abgesenkt. CurrentFolder/CurrentAccount starten bei einem Wechsel eine neue Remote-Suche; `CurrentFolderAndSubfolders` bleibt für Graph fail-closed lokal auf der geladenen Seite mit persistenter Unvollständigkeitswarnung. Die normale Schnellsuche gleicht AllAccounts sichtbar auf CurrentAccount ab; Advanced und Saved Search bleiben im exakten Scope lokal. Sichere Continuation-Cursor treiben „Mehr laden“ snapshotgebunden an und werden erst nach erfolgreichem Modellaufbau übernommen. Offen bleiben zusätzliche native Upstream-Prädikate, echte Camel-S-Expression-FreeForm, rekursive Graph-Subfolder-Suche, echtes Multi-Account-Graph, nicht sicher fortsetzbare Provider-Truncation sowie Linux-/EDS-/Graph-/Provider-Abnahme; gezielte Parser-, Advanced-Builder-, Negations-, Scope-, Codec-, Migrations-, Paging- und Saved-Search-Mutationstests sind vorhanden. |
| 66 | `GW-CAL-SHELL` | `P1-CAL-001` | `data/org.gnome.Evolution4.gschema.xml`, `src/services/{settings,migration}_service.rs`, `src/views/calendar_page.rs`, `src/window.rs` | App-eigener Kalender und Task/Memo-Snapshots | BLOCKED_LINUX | Upstream Taskpad & Memopad (`tag_vpaned` in `content_hpaned`), Toolbar-Umschalter (`calendar-show-tag-vpane`), Positionspersistenz (`calendar-tag-vpane-position`), synchrone Filterung (Query, Kategorie, versteckte Quellen), ECalService-Taskmutation sowie Open-by-UID-Routing über Window-Komponentenvertrag portabel implementiert und durch 54/54 Kalendertests sowie 19/19 Migrationstests belegt; native ECal-/EDS-Live-Provider-Abnahme unter Linux offen. |
| 67 | `GW-CONTACT-ACTIONS-DELETE-BATCH` | `P1-CONTACT-001` | `src/views/contacts_page.rs` | `GW-CONTACT-ACTIONS-BATCH-EXPORT` | BLOCKED_LINUX | 0/1/n-Auswahl, all-writable Preflight, named/count Confirmation, sequentielles Stop-on-first-error-Delete über EBookService und bestätigter Prefix/Failure/Unattempted-Reducer portabel implementiert und durch 22/22 Tests belegt; EBook-Batchmutation und Providerabnahme offen. |
| 68 | `GW-CONTACT-ACTIONS-PRINT` | `P1-CONTACT-001` | `src/services/contact_print_service.rs`, `src/views/contacts_page.rs` | `GW-CONTACT-ACTIONS-DELETE-BATCH` | BLOCKED_LINUX | Upstream-Parität zu `e-contact-print.c` und `e-addressbook-view.c:1650-1748` portabel implementiert: Einzel-/Mehrfachkarten mit Alphabet-Buchstaben-Headings, Adresslisten-Mitgliederauflösung/Verbergung, Tabellendruck sichtbarer Kontakte, striktes GLib-Markup-Escaping, 0/1/n-Sensitivität, Kontextmenü, Sidebar-Listendruck, `<Ctrl>P`-Shortcut und Fail-Closed-Preflight belegt durch 8 Print-Service-Tests, 23 Kontakt-Tests und 932/932 Workspace-Tests; Linux-Druckerdialog-/Portal-Abnahme offen. |
| 69 | `GW-CONTACT-ACTIONS-TRANSFER` | `P1-CONTACT-001` | `src/views/contacts_page.rs` | `GW-CONTACT-ACTIONS-PRINT` | BLOCKED_LINUX | Upstream-Parität zu `e-addressbook-view.c:2111-2265` und `eab-gui-util.c:511-570` portabel implementiert: Kontakttransfer zwischen Adressbüchern ("In Adressbuch kopieren…", "In Adressbuch verschieben…") für 1..n Kontakte, Dropdown-Auswahl schreibbarer Zieladressbücher per `adw::AlertDialog`, Frische-UID-Generierung (`prepare_vcard_for_transfer`), Create-Before-Delete bei Move, All-Writable-Source-Preflight für Move, Read-Only-Toleranz bei Copy, Stop-on-First-Error-Semantik, Detail-Buttons, Kontextmenü-Einträge, 0/1/n-Sensitivitätssteuerung und Reducer belegt durch 33/33 Kontakttests und 942/942 Workspace-Tests; Linux-/EDS-Backend-Laufzeitabnahme offen. |
| 70 | `GW-CONTACT-SEARCH` | `P1-CONTACT-001` | `src/services/contact_search_service.rs`, `src/dialogs/contact_search_dialog.rs`, `src/views/contacts_page.rs` | `GW-CONTACT-ACTIONS-TRANSFER` | BLOCKED_LINUX | Upstream-Parität zu `src/modules/addressbook/e-book-shell-view.c::book_shell_view_execute_search`, `e_addressbook_view_set_search` und `e-book-shell-content.c` portabel implementiert: typisierter AST (`ContactSearchQuery`), UND/ODER-Klauselgruppen (`All`/`Any`), Feld- und Operator-Registry (`ContactSearchField`, `ContactSearchOperator`), äußere Filter (Adressbuch-Quelle, Kategorie, Kontakte ohne Kategorie, Nur-Dubletten), `ContactSearchDialog` mit dynamischem Klausel-Editor und Syntax-Fallback, Schnellsuch-Modi (Alle Felder, Name, E-Mail, Telefon, Kategorie), asynchrone Worker-Suche mit Generierungszählung und Stornierung, automatische Treffer-/Preview-Synchronisation und stabile Selektionswiederherstellung belegt durch 22 Search-Tests, 35 Kontakt-Tests und 944/944 Workspace-Tests; EDS-Backend-Query-Syntax und Linux-Provider-Suchgrenzen offen. |
| 71 | `GW-EVENT-EDITOR` | `P1-CAL-001` | `src/dialogs/event_dialog.rs`, `src/services/calendar_store_service.rs` | `P1-CAL-001B`, `GW-CAL-SHELL` | BLOCKED_LINUX | Upstream-Parität zu `calendar/gui/e-comp-editor-event.c` und `e-comp-editor.c`: `update_existing_event_component` für verlustfreies Event-Editing, strikte Beibehaltung unberührter RFC5545-Eigenschaften (`ORGANIZER`, `GEO`, `URL`, `PRIORITY`, `X-EVOLUTION-*`, Custom-Properties), Parameter-Erhalt bei Teilnehmern (`CN`, `ROLE`, `PARTSTAT`) und Anhängen (`FMTTYPE`), Multi-VALARM-Synchronisation, Datums-/Uhrzeit- und Zeitzonenerhalt (`TZID`), robuster Wechsel zwischen Ganztags- (`VALUE=DATE`) und Uhrzeit-Modus, Eingabevalidierung und CRLF-Injektionsschutz belegt durch 13 Unit-Tests und 947/947 Workspace-Tests; nativer EDS-ECalClient-Server-Roundtrip unter Linux offen. |
| 72 | `GW-CALENDAR-SETTINGS` | `P1-CAL-001` | `data/org.gnome.Evolution4.gschema.xml`, `src/services/{settings,migration,backup}_service.rs`, `src/dialogs/preferences_dialog.rs`, `src/views/calendar_page.rs` | `P1-CAL-001B`, `GW-CAL-SHELL` | BLOCKED_LINUX | Upstream-Parität zu `calendar-config-keys.h`, `calendar-config.c` und `e-calendar-preferences.c`: Zeiteinteilung (`calendar-time-divisions`: 5/10/15/30/60 min), Wochenende-Komprimierung (`calendar-compress-weekend`), wochenweises Scrollen (`calendar-month-scroll-by-week`), zweite Zeitzone (`calendar-second-zone`) samt Verlaufsliste (`calendar-second-zones`), Unterstützung von `year` und `list` in allen Einstellungs- und Validierungsschichten, Live-Synchronisation in Kalenderansicht, vollständige Upstream-Dconf/Ini-Migration sowie versioniertes Backup/Restore belegt durch 950/950 Workspace-Tests; GNOME Desktop GSettings-Integration und Visuals unter Linux offen. |
| 73 | `GW-SOURCE-SETTINGS` | `P1-CAL-001` | `data/org.gnome.Evolution4.gschema.xml`, `src/services/{settings,migration,backup}_service.rs` | `GW-SOURCE-SELECTOR`, `GW-CALENDAR-SETTINGS` | BLOCKED_LINUX | Upstream-Parität zu `e-source-selector.c` und `calendar-config-keys.h`: Default-Quellen für Kalender, Aufgaben, Memos und Adressbücher, domänenspezifische Sichtbarkeitslisten (`task-hidden-sources`, `memo-hidden-sources`, `addressbook-hidden-sources`), Gruppen-Sichtbarkeit und Gruppen-Reihenfolge (`calendar-hidden-groups`, `calendar-groups-order`, `task-hidden-groups`, `task-groups-order`, `memo-hidden-groups`, `memo-groups-order`, `addressbook-hidden-groups`, `addressbook-groups-order`), atomares Pruning gelöschter UIDs (`prune_deleted_source`), Upstream-Dconf-Migration samt Key-Normalisierung und GVariant-Array-Dekodierung sowie versioniertes Backup/Restore mit strikter Validierung belegt durch 20 Settings-, 21 Migrations- und 24 Backup-Tests sowie 953/953 Workspace-Tests; EDS-Source-Gruppen und Live-Provider auf Linux offen. |
| 74 | `GW-REMINDER-SETTINGS` | `P1-CAL-001`, `P1-TASK-001` | `data/org.gnome.Evolution4.gschema.xml`, `src/services/{settings,migration,backup}_service.rs`, `src/dialogs/{event,preferences}_dialog.rs`, `src/views/{tasks,calendar}_page.rs` | `GW-ALARMS`, `GW-TASK-FILTERS`, `GW-CALENDAR-SETTINGS` | BLOCKED_LINUX | Upstream-Parität zu `calendar-config-keys.h`, `calendar-config.c`, `e-calendar-preferences.c` und `org.gnome.evolution.calendar.gschema.xml.in`: Standard- und Geburtstags-/Jahrestagserinnerungen (`use-default-reminder`, `default-reminder-interval`, `default-reminder-units`, `use-ba-reminder`, `ba-reminder-interval`, `ba-reminder-units`), Aufgaben- und Termin-Filtereinstellungen (`hide-completed-tasks`, `hide-completed-tasks-value`, `hide-completed-tasks-units`, `hide-cancelled-tasks`, `hide-cancelled-events`), Upstream-Sexpression-Schwellenwertfilterung (`now - value * unit_in_seconds`), Default-Alarm-Policy (nur neue Termine, keine Überschreibung bei Edits), Live-GSettings-Sync in Aufgaben- und Kalenderseite, Preferences-UI sowie versionierte Backup- und Migrationsparität belegt durch 958/958 Workspace-Tests; EDS-Kalender-/Alarm-Daemon auf Linux offen. |
| 75 | `GW-ADDRESSBOOK-SETTINGS` | `P1-CONTACT-001` | `data/org.gnome.Evolution4.gschema.xml`, `src/services/{settings,migration,backup}_service.rs`, `src/dialogs/preferences_dialog.rs`, `src/views/contacts_page.rs` | `GW-CONTACT-SEARCH`, `GW-SOURCE-SETTINGS` | BLOCKED_LINUX | Upstream-Parität zu `eab-config.c`, `autocompletion-config.c`, `e-preferences-window.c` und `org.gnome.evolution.addressbook.gschema.xml.in`: Autocomplete-Einstellungen (`contact-completion-minimum-query-length`, `contact-completion-show-address`, `contact-completion-book-sources`), Darstellung &amp; Karten (`contact-address-formatting`, `contact-preview-show-maps`, `contact-preview-home-before-work`, `contact-open-map-target`, `contact-open-map-prefer-local`), Ansichtslayout &amp; Panes (`contact-layout`, `contact-preview-visible`, `contact-hpane-position`, `contact-vpane-position`), 12 Editor-Sektionstoggles (`contact-editor-show-*`), Fallback-Karten-URL-Generierung (`build_contact_map_uri`), prioritäre Privat-/Geschäftsadressensortierung (`ordered_contact_addresses`), Live-GSettings-Sync, Preferences-UI-Gruppen sowie versioniertes Backup/Restore und Upstream-Dconf-Migration belegt durch 963/963 Workspace-Tests; Linux-Desktop-Kartenhandler- und EDS-Daemon-Integration offen. |

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
  Großpostfach-Abnahme sowie native Expunge-/Filterwirkung und
  Graph-/Account-Environment. Die Sidecar-Writer-Serialisierung und die
  typisierte Global-Search-Aktivierungsidentität sind portabel geschlossen.
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

### MAIL-LIST-VIEW-CONTRACT — Evolutions Listenprofile und Spaltenordnung

- Status: `BLOCKED_LINUX`; der portable Vertrag ist geschlossen.
- `Messages` beginnt wie die Originalansicht mit Status, Anhang, Markierung,
  Absender, Betreff und Datum. `Wide` beginnt mit Status, Markierung und einer
  zweizeiligen Nachrichtenzelle aus Absender und Betreff.
- Jede gespeicherte Reihenfolge wird auf bekannte, eindeutige und vollständige
  Spalten normalisiert. Manuelles Verschieben erzeugt das Profil `custom`;
  Profilwechsel stellt die deterministische Originalreihenfolge wieder her.
- Kopf und Datenzeile werden aus derselben ID-Reihenfolge gebaut. Änderungen
  ordnen den vorhandenen Kopf neu und erzeugen nur Listenzellen neu; Mail-
  Provider, Filter, Paging und Auswahl werden nicht neu geladen.
- Profil, Reihenfolge sowie Absender- und Datumsbreite sind in Schema,
  Preferences, Spalten-Popover, Migration und Backup/Restore enthalten.
- Linux-Abnahme: Header-/Zeilenausrichtung, Resize/Restart, Tastatur/A11y,
  Drag-and-drop und große Threads/Postfächer; siehe `ROADMAP_LINUX.md`.

### MENU-ACTION-MAP — gemeinsamer Shell-Aktionsvertrag

- Status: `BLOCKED_LINUX`; Menü-, Shortcut- und Zustandsvertrag sind portabel
  geschlossen.
- `action_contract.rs` ist die eindeutige Liste user-sichtbarer täglicher
  `app.*`-/`win.*`-Actions mit Scope. Das primäre File/Edit/View/Message/
  Folder/Tools/Help-Menü und das Transportmenü werden daraus aufgebaut.
- Die Shortcut-Hilfe enthält nur Vertragseinträge und bezieht den sichtbaren
  Accelerator direkt aus der tatsächlich registrierten Anwendungstabelle.
- Tests verwerfen unbekannte oder doppelte IDs/Platzierungen, fehlende
  Accelerators und fehlende App-/Window-Registrierungen. Bei leerer
  Nachrichtenauswahl werden alle Message-Scopes deaktiviert, ohne Suche,
  Modulwechsel oder New zu sperren.
- Linux-Abnahme: Popup-Vollabgleich, GTK-/IME-/Fokusdispatch, Theme/A11y und
  reale Provider-/Folder-Capabilities.

### MAIL-SEARCH-CONTRACT — kanonische Suchsyntax und Saved Searches

- Status: `BLOCKED_LINUX`
- Abgeschlossen sind die strikte kanonische Syntaxvalidierung neuer Saved
  Searches, die fail-closed Unknown-State-Negation sowie die geschützten
  Saved-Search-Mutationen gegen korrupte/unsupported/duplizierte Dateien mit
  eindeutigen temporären Dateien, atomaren Writes und process-lokaler
  RMW-Serialisierung sowie exakte `CurrentFolder`/`CurrentFolderAndSubfolders`/
  `CurrentAccount`/`AllAccounts`-Scopes auch bei leerer Suchabfrage. Die sieben
  sichtbaren Upstream-Schnellmodi `SubjectOrAddresses`, `Subject`, `Sender`,
  `Recipients` (To/Cc, ohne Bcc), `Message`, `Body` und `FreeForm` verwenden
  denselben kanonischen AST und Matcher; MailPage verwendet ihn ausschließlich.
  Der Graph-Transport senkt nur strict-safe positive Konjunktionen ab; OR/NOT
  und local-only-Prädikate werden als Ganzes nicht remote abgesenkt.
  CurrentFolder/CurrentAccount lösen bei einer laufenden Graph-Suche jeweils
  eine neue Remote-Suche aus; `CurrentFolderAndSubfolders` bleibt für Graph
  fail-closed lokal auf der geladenen Seite und zeigt dauerhaft die
  Unvollständigkeitswarnung. Die normale Schnellsuche gleicht AllAccounts
  sichtbar auf CurrentAccount ab; Advanced und Saved Search behalten den
  exakten Scope und bleiben dort lokal. Das Graph-Suchergebnis trägt
  `is_complete`; sichere, origin-geprüfte Continuation-Cursor treiben
  „Mehr laden“ snapshotgebunden an und werden erst nach erfolgreichem
  Modellaufbau übernommen. Bei Provider-Truncation ohne sicheren Cursor zeigt
  die UI weiter eine sichtbare Unvollständigkeitswarnung.
- Der mail-sensitive portable Advanced-Search-Dialog erzeugt Text-/Status-/
  Datumsregeln mit UND/ODER sowie Regel-/Gruppen-Negation, validiert sie gegen
  denselben Parser und erhält Scope und exaktes Ordnerziel. Offen bleiben
  zusätzliche native Upstream-Prädikate, echte Camel-S-Expression-FreeForm,
  rekursive Graph-Subfolder-Suche, echtes Multi-Account-Graph, nicht sicher
  fortsetzbare Provider-Truncation und die Linux-/EDS-/Graph-/Provider-Abnahme.
  Saved-Search-v2 speichert Feld, Scope, exakte Konto-/Ordneridentität und den
  versionierten kanonischen AST, liest v1 deterministisch weiter und lässt
  fehlende Ziele mit sichtbarer Warnung und 0 Treffern fail-closed. Auswahl und
  Zähler verwenden denselben Compiler; relative Datumswerte bleiben dynamisch.
  Gezielte Parser-, Negations-, Codec-, Migrations- und
  Saved-Search-Mutationstests begleiten den Slice; eine Gesamt-Testzahl wird
  hier nicht behauptet.

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
  large-mailbox acceptance, native Expunge/Filter und Graph/account
  environment. Sidecar-Writer-Serialisierung und type-aware Global-Search-
  activation identity sind portabel geschlossen.

### P0-MAIL-SIDECAR-SERIALIZATION

- Status: `DONE`; Produktionsscope ausschließlich
  `src/services/mail_metadata_service.rs`.
- Mailmetadaten und Freigaben externer Inhalte halten jetzt eine gemeinsame,
  crash-freigegebene Geschwister-Lockdatei über die vollständige
  Read-Modify-Write-Transaktion. Der vorhandene atomare Temp+Rename-Ersatz der
  Datendatei bleibt erhalten; ein Prozess-Mutex schließt zusätzlich
  plattformabhängige Advisory-Lock-Lücken zwischen Store-Instanzen.
- Belegt: konkurrierende unabhängige Writer verlieren weder disjunkte
  Nachrichtenstände noch Remote-Freigaben oder eine gleichzeitige
  Widerrufung. Nicht-`NotFound`-Lesefehler brechen ohne Ersatz der Zieldatei
  ab. Der fokussierte Service-Lauf ist 15/15 grün; der aktuelle vollständige
  portable Workspace-Lauf ist 796/796 grün.

### MAIL-DELETE-RECOVERY

- Status: `BLOCKED_LINUX`; der app-eigene lokale Wiederherstellungspfad ist
  implementiert und getestet.
- `Ctrl+Shift+D`, Edit-/Message-Menü und Nachrichtenkontext verwenden
  `win.undelete`. Die persistente View-Option materialisiert nur lokal im
  Sidecar gelöschte Nachrichten in ihrem ursprünglichen logischen Ordner,
  kennzeichnet sie durchgestrichen und zählt sie nicht als ungelesen. Das
  Anzeigen führt diese Zeilen ausdrücklich nicht erneut durch Incoming-Filter.
- Eine ausgewählte lokale gelöschte Einzel- oder Mehrfachauswahl wird über
  eine neue exakte reversible Restore/Delete-Operation wiederhergestellt. Ohne
  solche Auswahl und ohne expliziten Batch bleibt der Fallback auf einen
  bestätigten Delete-History-Head begrenzt; laufende Mailoperationen sperren
  die Action. Lokales Trash-Routing stellt explizit zur Quelle `local` wieder
  her.
- Grenze: Native Camel-/Graph-Deleted-Flags, auswählbare Providerzeilen,
  Provider-Wiederherstellung, Expunge-Wechselwirkung und sichtbare Linux-
  Abnahme bleiben offen.

### FOLDER-CONTEXT-TARGET

- Status: `BLOCKED_LINUX`; der portable Target-Vertrag ist implementiert und
  getestet.
- Ein Sekundärklick bindet den exakten Ordner nur für die Lebensdauer des
  Popovers, ohne aktive Leser-/Load-Auswahl zu ändern. Leeraum,
  nicht-selektierbare Zeilen und Account-Wurzeln retargeten nicht. Das Popup
  enthält nur Aktionen, die diesen Target-Vertrag bereits sicher verwenden:
  Umbenennen, Löschen, Alle-gelesen und Eigenschaften.
- Ungeladene Ordner werden beim popup-spezifischen Alle-gelesen explizit
  abgelehnt statt als erfolgreicher Null-Lauf gemeldet. Offen bleiben reale
  GTK4-/HiDPI-Gesture-Abnahme und providerneutrale Copy/Move/Refresh/
  Subscription-Mutationen.

### MAIL-FOLDER-SORT-ORDER

- Status: `BLOCKED_LINUX`; der portable Sortiervertrag ist implementiert und
  getestet.
- Ein persistenter, validierter JSON-GSettings-Vertrag ordnet nur Geschwister.
  Die lokale oberste Ebene umfasst Standard- und Custom-Ordner; das Aggregat
  „Alle Nachrichten“ bleibt fest. Geladene Kontoordner verwenden stabile
  Source-/Folder-Identitäten. Standardmäßig bleibt Inbox zuerst, explizite
  Ränge dürfen dies wie upstream überschreiben.
- Der GTK4-Dialog verschiebt innerhalb genau einer Ebene und setzt entweder
  diese Ebene oder alle Ebenen zurück. Sidebar-Rebuild und Auswahlrestoration
  starten keinen Mail-Load und mutieren keinen Provider. Migration, Reset und
  Backup-Roundtrip sind abgedeckt; offen bleibt die Linux-/HiDPI-Interaktion.

### MAIL-FOLDER-PROPERTIES-SNAPSHOT

- Status: `BLOCKED_LINUX`; der factual read-only Snapshot ist portabel
  implementiert und getestet.
- Nur echte geladene lokale Ordner oder exakt passende selektierbare
  `CamelFolderSummary`-Einträge liefern Eigenschaften. Counts, Accountname und
  menschlicher Ordnerpfad stammen aus diesem Snapshot; Aggregat-/Suchordner,
  Account-Wurzeln und ungeladene Remote-Ziele bleiben deaktiviert. Das
  rechtsgeklickte Popup-Ziel gilt nur für dessen Lebensdauer.
- Die frühere synthetische Abonnementliste mit verworfenen Checkbox-Änderungen
  ist entfernt. Offen bleiben native Quota-/per-Folder-Properties; reale
  Abonnements sind im getrennten Paket `MAIL-CAMEL-SUBSCRIPTIONS` belegt.

### MAIL-CAMEL-SUBSCRIPTIONS

- Status: `BLOCKED_LINUX`; Bridge und GTK4-Vertrag sind implementiert und
  getestet.
- Ein exakter geladener nativer Camel-Source-UID wird auf
  `CamelSubscribable` geprüft. Die rekursive `SUBSCRIPTION_LIST` liefert
  Providername, vollständigen Ordnernamen, Parent, Counts, `NOSELECT` und den
  autoritativen Abonnementstatus; Nicht-Unterstützung bleibt von List-/Netz-
  und Authentifizierungsfehlern unterscheidbar.
- Dialog-Laden, Refresh, Mutation und Cancel laufen außerhalb des GTK-Kontexts
  und sind serialisiert sowie generationsgesichert. Checkboxen zeigen während
  einer Mutation weiterhin den bestätigten Zustand; erst ein erfolgreicher
  Subscribe/Unsubscribe-Aufruf plus frische Providerliste aktualisiert Dialog
  und Sidebar. Graph/lokale Stores erhalten keinen Fallback.
- Linux-Rest: Live-IMAP-/NNTP-Konten, Auth-/Offline-/Cancel-Verhalten,
  Sidebar-Reconciliation und GTK4-/HiDPI-Visuals.

### MAIL-THREAD-CONTEXT-CACHE

- Status: `BLOCKED_LINUX`; der portable Auswahl-/Cache-Vertrag ist
  implementiert und getestet.
- Der Reader rendert den kompakten Konversationskontext aus der bereits
  geordneten `thread_members`-Map der aktuell publizierten Load-Generation.
  Während eines Threadmodell-Neuaufbaus bleibt der Kontext verborgen; erst
  nach atomarem Ersatz der Membership-/Children-Maps wird die Generation
  freigegeben und eine erhaltene GTK-Auswahl ausdrücklich aktualisiert.
- Der Auswahlpfad scannt oder sortiert den flachen Nachrichtenstore nicht
  mehr. Offen bleiben WebKitGTK-/GTK4-Visuals, reale Großpostfächer und der
  Vergleich der kompakten Platzierung mit Evolution auf Linux.

### GW-CAL-VIEWS-001

- Status: `BLOCKED_LINUX`; der portable Schnitt in
  `src/views/calendar_page.rs` ist geschlossen und reviewt.
- Belegt: halb-offene Day/Week/WorkWeek/Month42/Agenda90/Year/List-Ranges,
  deterministische Navigation, immutable Range-Index, Generation/Cancel/
  Stale-Gates sowie DTEND-exklusive DST-/Recurrence-Regeln. Selection und
  globale Aktionen bleiben außerhalb dieses portablen View-Schnitts.
- Linux-Handoff: native EDS-Range-Abfragen müssen dieselben Records für die
  halb-offenen Bereiche liefern; danach ECal-Monitor-/Provider-Refresh, reale
  Zeitzonen/DST/Recurrence, sieben Views und Visuals prüfen. Selection und
  globale Aktionen bleiben separate offene Implementierung.

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

### MAIL-ACCELERATOR-CONTRACT

- Status: `BLOCKED_LINUX`; der portable Action-/Shortcut-Vertrag ist geschlossen.
- Portabler Beleg: eine zentrale konfliktfreie Accelerator-Tabelle bindet die
  Upstream-Wege für nächste/vorherige und nächste/vorherige ungelesene Mail,
  Preview, Threading, Print, Source und Find. Menü und Shortcuts-Fenster verwenden
  dieselben Actions; Empty-/No-selection-/Boundary-/Wrap-Fixtures sind grün.
- Linux-Rest: GTK4-Acceleratoranzeige, IME-/Portal-Fokus und echte Listen-/WebKit-
  Fokusübergänge mit Tastaturlayouts abnehmen.

### GW-CAL-ACTIONS-TRANSFER-DELETE

- Status: `BLOCKED_LINUX`; portabler Teil des offenen Elternpakets
  `GW-CAL-ACTIONS` geschlossen.
- Portabler Beleg: explizites Copy/Move nach Lokal, create-before-remove,
  authoritative Reload nach jedem Teilfehler sowie Serien-Löschumfang mit
  fail-closed This-Occurrence-/This-and-Future-Pfaden und Pure-Plan-Tests.
- Offener Parent-/Linux-Rest: beliebige EDS-Ziele, verlustfreie EXDATE-/Serien-
  Split-Mutation, Forward/RSVP/Schedule/Properties/Print und Provider-ACLs.

### GW-CAL-SHELL

- Status: `BLOCKED_LINUX`; portabler Shell-, Layout-, Filter- und Routing-Vertrag des offenen Parents `P1-CAL-001` geschlossen.
- Portabler Beleg: Upstream-äquivalenter horizontal geteilter Kalenderarbeitsbereich (`tag_vpaned` in `content_hpaned`) mit Taskpad (Aufgabenliste, Zählerbadge, Statuslabel, Direktanlage-Aktion) und Memopad (Notizenliste, Zählerbadge, Statuslabel, Direktanlage-Aktion).
  - Toolbar-Toggle (`tag_vpane_toggle_btn`) gebunden an GSettings-Key `calendar-show-tag-vpane` (Default: `true`) und `tag_vpaned`-Sichtbarkeit.
  - Horizontale/vertikale Paned-Positionierung gebunden an `calendar-tag-vpane-position` (Default: `320`) mit Live-Persistenz und Migrationsabbildung von upstreams `tag-vpane-position` und `show-tag-vpane`.
  - Synchrone Kreuzmodul-Filterung: Eingaben in `calendar_search_entry` und Auswahlen in `calendar_category_selector` filtern Termine, Aufgaben (Taskpad) und Notizen (Memopad) parallel und deterministisch unter Berücksichtigung von `calendar-hidden-sources`.
  - Open-by-UID-Routing: `CalendarPage::open_search_result` löst UIDs konsistent über Termine (Event-Editor), Aufgaben (`open_task_by_uid` -> Task-Editor) und Notizen (`open_memo_by_uid` -> Memopad/Memo-Editor) auf.
  - Abnahme: 54/54 Kalendertests, 19/19 Migrationstests und vollständige 916/916-Workspace-Suite grün.
- Offener Parent-/Linux-Rest: ECal-Live-Signalmonitore, native EDS-Daemon-Synchronisation für VTODO/VJOURNAL-Updates und native Linux-Visualisierung.

### GW-CAL-ACTIONS-PURGE-LOCAL

- Status: `BLOCKED_LINUX`; der app-eigene portable Teil des offenen Parents
  `GW-CAL-ACTIONS` ist geschlossen.
- Portabler Beleg: kalendergebundenes `Ctrl+E`, upstream-identischer
  60-Tage-Default und 0..1000-Tage-Instant-Cutoff, destruktiver Dialog mit
  Worker-/Cancel-/Doppelstart-Gates, einmaliger atomarer Rewrite pro lokaler
  Datei, bestätigte Partial-Ergebnisse, Toast und autoritativer Reload. Nur
  eindeutig nicht-serielle VEVENTs mit Ende vor dem Cutoff werden entfernt;
  Serien, Detached Instances, Duplicate UIDs, DURATION und ungültige Grenzen
  bleiben sichtbar gezählt und unverändert.
- Linux-Rest: Upstreams aktuell aktive, nicht-read-only ECal-Clients exakt
  snapshotten, vollständige Instanzen gegen den Cutoff auswerten, mit korrektem
  MOD_THIS/MOD_ALL-Scope sequentiell entfernen und Cancel/Providerfehler samt
  bestätigtem Prefix und ECal-Monitor-Reload abnehmen. Refresh ist kein Purge.

### GW-CONTACT-ACTIONS-BATCH-EXPORT

- Status: `BLOCKED_LINUX`; portabler Teil des offenen Elternpakets
  `GW-CONTACT-ACTIONS` geschlossen.
- Portabler Beleg: echte 0/1/n-Auswahl, Batch-Mail, selection-sensitive Actions,
  exakte Context-Selection und vor dem FileDialog vollständig validierter,
  atomar ersetzter gemeinsamer vCard-3/4-Payload.
- Offener Parent-/Linux-Rest: Batch-Delete/Transfer/Merge-Rollback, Printlayout,
  File-Portal und reale EBook-/EDS-ACL-/Providerwirkung.

### GW-CONTACT-ACTIONS-DELETE-BATCH

- Status: `BLOCKED_LINUX`; portabler Batch-Löschen-Teil des offenen Elternpakets
  `GW-CONTACT-ACTIONS` geschlossen.
- Portabler Beleg: 0/1/n-Auswahl preflightet alle ausgewählten Kontakte auf
  schreibfähige Quellen (`contact_source_is_writable`); schreibgeschützte Quellen
  deaktivieren das Löschen mit erklärendem Tooltip. Ein Bestätigungsdialog fragt
  bei 1 Kontakt mit Name und Adressbuchquelle und bei n Kontakten mit der genauen
  Anzahl. Der Worker löscht sequentiell außerhalb des GTK-Hauptloops über
  `EBookService::remove_contact` und stoppt beim ersten Fehler. Der Reducer
  entfernt nur den bestätigten gelöschten Präfix aus dem UI-Modell und der
  Kontaktliste; unattempted und fehlgeschlagene Kontakte bleiben erhalten.
  Entf/Rücktaste auf der Kontaktliste lösen das Löschen bei aktiver Sensitivität
  aus, und Strg+A markiert alle sichtbaren Kontakte. Belegt durch 22/22
  Kontakt-Unit-Tests sowie 923/923 Workspace-Tests.
- Offener Parent-/Linux-Rest: EBook-Batchmutation, server-gestützte
  Transaktionsgarantie, Undo und native Linux-/Providerabnahme.

### GW-CONTACT-ACTIONS-PRINT

- Status: `BLOCKED_LINUX`; portabler Drucken-Teil des offenen Elternpakets
  `GW-CONTACT-ACTIONS` geschlossen.
- Portabler Beleg: Parität zu Upstream GNOME Evolution `e-contact-print.c` und
  `e-addressbook-view.c`:
  1. Kartendruck (`build_contact_print_document`): Für einen oder mehrere ausgewählte
     Kontakte formatiert als sauberes HTML und formatierter Plain-Text-Fallback.
     Mehrere Kontakte werden nach Nachname/Vorname/Anzeigename alphabetisch sortiert
     und mit auffälligen Buchstaben-Headings (`A`, `B`, `C`...) gruppiert.
     Alle Felder (Name, Organisation, Titel, E-Mails, Telefone, Adressen, Kategorien,
     Adressbuchquelle, Notizen) werden strikt via `glib::markup_escape_text` gegen XSS
     und Injektionen geschützt. Kontaktlisten/Verteilerlisten rendern einen eigenen
     Mitgliederabschnitt (mit Auflösung oder Verbergung gemäß `list_show_addresses`).
  2. Tabellendruck (`build_contact_list_print_document`): Die aktuell sichtbare/gefilterte
     Kontaktliste wird als tabellarische Übersicht (Name, Firma/Position, E-Mail, Telefon,
     Adressbuch, Kategorien) mit Zusammenfassungskopf gedruckt.
  3. UI-Integration in `ContactsPage`:
     - Detail-Header `actions_box`: `print_btn` (`document-print-symbolic`) mit dynamischem
       Tooltip für 0, 1 oder n Kontakte.
     - Sidebar `new_contact_bar`: `print_list_btn` ("Sichtbare Kontakte drucken").
     - Kontextmenü: "Drucken…" Eintrag auf Kontaktzeilen.
     - Tastenkombination `<Ctrl>P`: Löst Kartendruck bei vorhandener Auswahl aus, sonst
       den Tabellendruck der sichtbaren Liste.
     - Informative Linux-Portal-Handoff-Meldung per Toast.
  4. Preflight: Leere Auswahlen/Listen und doppelte Kontakt-Identitäten werden fail-closed
     abgelehnt.
  Belegt durch 8 Unit-Tests in `contact_print_service::tests`, 23 Tests in `contacts_page::tests`
  sowie 932/932 Workspace-Tests.
- Offener Parent-/Linux-Rest: Nativer Linux-Druckerdialog via GTK PrintOperation /
  XDG Desktop Portal Druckportal, Page-Setup-Persistenz und visuelle Druckbild-Abnahme unter Linux.

### GW-CONTACT-ACTIONS-TRANSFER

- Status: `BLOCKED_LINUX`; portabler Transfer-Teil des offenen Elternpakets
  `GW-CONTACT-ACTIONS` geschlossen.
- Portabler Beleg: Parität zu Upstream GNOME Evolution `e-addressbook-view.c:2111-2265`
  (`e_addressbook_view_copy_to_folder`, `e_addressbook_view_move_to_folder`) und `eab-gui-util.c:511-570`
  (`eab_transfer_contacts`):
  1. Transfer-Arten:
     - **In Adressbuch kopieren…** (`ContactTransferKind::Copy`): Kopiert 1..n Kontakte in das
       gewählte schreibbare Zieladressbuch. Darf auch aus schreibgeschützten Quellen ausgeführt werden.
     - **In Adressbuch verschieben…** (`ContactTransferKind::Move`): Create-before-delete Semantik.
       Zielkontakt wird neu angelegt; erst nach verifizierter Backend-Bestätigung wird der Quellkontakt
       entfernt. Stop-on-first-error stoppt sofort bei erstem Fehler, ohne weitere Datensätze zu löschen.
       Strikt fail-closed deaktiviert, sobald mindestens ein Quellkontakt aus einer schreibgeschützten
       Quelle stammt.
  2. Adressbuch-Auswahldialog & Preflight:
     - Zieladressbuch-Auswahl via `adw::AlertDialog` mit `gtk::DropDown`, das alle schreibbaren
       Adressbücher anzeigt (unter Ausschluss des gemeinsamen Quelladressbuchs).
     - Preflight prüft: Mindestens 1 Kontakt ausgewählt, Ziel schreibbar, Quelle != Ziel,
       keine doppelten Identitäten in der Auswahl.
  3. Kollisionsfreie vCards (`prepare_vcard_for_transfer`):
     - Ersetzt vorhandene `UID:...` durch frische `UID:evolution-gtk4-<uuid>` (oder fügt nach `BEGIN:VCARD` ein),
       sodass keine `AlreadyExists`-Konflikte beim Kopieren/Verschieben auftreten.
  4. UI-Integration in `ContactsPage`:
     - Detail-Header `actions_box`: `copy_to_book_btn` ("In Adressbuch kopieren…") und
       `move_to_book_btn` ("In Adressbuch verschieben…") mit 0/1/n-Sensitivität und Tooltips.
     - Kontextmenü: "In Adressbuch kopieren…" und "In Adressbuch verschieben…" Einträge.
     - Off-GTK-Thread Worker mit GTK-Loop Response-Handling und selektivem Modell-Update.
  Belegt durch 10 neue Transfer-Unit-Tests in `contacts_page::tests` (33/33 Tests gesamt)
  sowie 942/942 Workspace-Tests.
- Offener Parent-/Linux-Rest: Native EDS EBook-Client-Batch-Mutationen, serverseitiges Atomic-Move,
  und Live-Laufzeitabnahme unter Linux mit echten Adressbuchquellen.

### GW-CONTACT-SEARCH

- Status: `BLOCKED_LINUX`; portabler Kontaktsuch- und Advanced-Search-Teil des offenen Elternpakets
  `P1-CONTACT-001` geschlossen.
- Portabler Beleg: Parität zu Upstream GNOME Evolution `src/modules/addressbook/e-book-shell-view.c::book_shell_view_execute_search`,
  `e_addressbook_view_set_search` und `e-book-shell-content.c`:
  1. Typisierter AST und Registry (`src/services/contact_search_service.rs`):
     - `ContactSearchQuery` mit Klauselgruppen `ContactClauseGroup::All` (UND) und `ContactClauseGroup::Any` (ODER).
     - Feld-Matrix: `Name`, `Email`, `Phone`, `Category`, `AnyField`.
     - Operator-Matrix: `Contains`, `NotContains`, `Is`, `IsNot`, `StartsWith`, `EndsWith`.
     - Äußere Filter (`ContactOuterFilters`): Adressbuchquelle (`source_uid`), exakte Kategorie (`category`),
       Kontakte ohne Kategorie (`unmatched_category`) und Dublettenfilter (`duplicate_only`).
     - Strikte Validierung (`validate`), versionierter Fixture-Codec (`encode_query_fixture` / `decode_query_fixture`),
       deterministische Normalisierung (`normalize_search_text`) und Fail-Closed-Syntaxprüfung.
  2. Dialog für erweiterte Kontaktsuche (`src/dialogs/contact_search_dialog.rs`):
     - `ContactSearchDialog`: Libadwaita-Fenster mit dynamischem Hinzufügen und Entfernen von Klauselzeilen,
       Feld- und Operator-Dropdowns, UND/ODER-Gruppenauswahl, äußerem Kategoriefeld, Checkbuttons für
       unmatched und Dubletten, Re-Open-Seeding (`seed_query_for_reopen`) aus dem aktiven Zustand.
     - Freitext-/Syntax-Fallback: typisierte Syntax wird akzeptiert, native GTK3-Ausdrücke und S-Expressions
       werden fail-closed abgelehnt.
  3. UI-Integration in `ContactsPage` (`src/views/contacts_page.rs`):
     - Schnellsuchleiste mit Textentry und Dropdown ("Alle Felder", "Name", "E-Mail", "Telefon", "Kategorie").
     - Button "Erweiterte Suche…" öffnet `ContactSearchDialog`.
     - Äußere Filter in der Sidebar: Adressbuchliste, Kategorie-Dropdown, "Nur ohne Kategorie", "Nur Dubletten".
     - Asynchroner Such-Worker (`start_contact_search`) mit Generierungszähler und Stornierungstoken.
     - Treffer- und Preview-Synchronisation: `apply_contact_search_selection` behält die Auswahl bei Treffer
       oder leert die Detailansicht sauber; Statusmeldung ("Keine Kontakte entsprechen der Suche") und Spinner.
  Belegt durch 22 Search-Tests in `contact_search_service` und `contact_search_dialog`, 35 Kontakttests in
  `contacts_page::tests` sowie 944/944 Workspace-Tests.
- Offener Parent-/Linux-Rest: EBook-Query-Syntax für native EDS-Backends, serverseitige Provider-Suchgrenzen
  und Live-Laufzeitabnahme unter Linux.

### GW-EVENT-EDITOR

- Status: `BLOCKED_LINUX`; portabler Ereignis-Editor- und Komponentenmutations-Teil des offenen Elternpakets `P1-CAL-001` geschlossen.
- Portabler Beleg: Upstream-Parität zu `calendar/gui/e-comp-editor-event.c` und `e-comp-editor.c`:
  1. Extrahierte, zustandsunabhängig testbare Mutationsfunktion (`update_existing_event_component` in `src/dialogs/event_dialog.rs`):
     - Unberührte RFC5545-Top-Level- und Vendor-Eigenschaften bleiben strikt erhalten: `ORGANIZER`, `GEO`, `URL`, `PRIORITY`, `X-EVOLUTION-MOVE-CALENDAR`, `X-CUSTOM-*` und benutzerdefinierte Flags.
     - Parameter-Erhalt bei Teilnehmern (`replace_ical_attendees`): Bestehende `CN`-, `ROLE`-, `PARTSTAT`- und `RSVP`-Parameter werden bei Übereinstimmung der Mail-Adresse verlustfrei erhalten; neu hinzugefügte Teilnehmer werden mit Standardpräfix serialisiert; entfernte Teilnehmer werden sauber getrimmt.
     - Parameter-Erhalt bei Anhängen (`replace_ical_attachments`): `FMTTYPE`-, `VALUE=URI`- und inline/remote-Attribute bleiben erhalten; neue Anhänge werden angehängt; gelöschte Anhänge werden entfernt.
     - Zeitzonen-Präfixe (`DTSTART;TZID=...`, `DTEND;TZID=...`, `RDATE;TZID=...`): Bestehende Zeitzonenbezeichner werden bei Zeitänderungen erhalten.
     - Wechsel zwischen Ganztags- (`VALUE=DATE`) und Uhrzeit-Modus: Bei Konvertierung zu Ganztag wird `VALUE=DATE` gesetzt und Zeitzonensuffixe bereinigt; bei Konvertierung zurück zu Uhrzeit wird `VALUE=DATE` sauber entfernt.
     - Multi-`VALARM`-Synchronisation: Integriert mit `AlarmEditor` und `replace_component_alarms`; benutzerdefinierte und erweiterte Alarmtypen (Audio, Email, Procedure) werden geschützt.
  2. Dialog- und Speicherintegration (`EventDialog::build_ui`):
     - Beim Speichern modifizierter Termine (`edit_uid.is_some()`) führt `EventDialog` `update_existing_event_component` aus und sendet die aktualisierte iCalendar-Zeichenkette asynchron über `ECalService::modify_component`.
     - Strikte Eingabevalidierung vor Mutation: Titel-Pflichtfeld, Beginn-vor-Ende-Prüfung, Statusvalidierung und Schutz gegen CRLF-Zeilenumbruch-Injektionen in Wiederholungs-, Teilnehmer- und Konferenzfeldern.
     - Automatische Persistenz von Kategorie-Metadaten über `CalendarCategoryStore`.
  Belegt durch 13 gezielte Unit-Tests in `dialogs::event_dialog::tests` (inkl. `update_existing_event_preserves_unknown_properties_and_parameters`, `update_existing_event_validates_inputs`, `update_existing_event_switches_all_day_mode`) und 947/947 Workspace-Tests.
- Offener Parent-/Linux-Rest: ECal-Create/Modify/Remove gegen Live-EDS-Dämonen, serverseitige iTIP-Planung/Meeting-Einladungen, Server-ACLs und Linux-Visuals.

### GW-CALENDAR-SETTINGS

- Status: `BLOCKED_LINUX`; portabler Einstellungs- und Migrations-Teil des offenen Elternpakets `P1-CAL-001` geschlossen.
- Portabler Beleg: Upstream-Parität zu `src/calendar/gui/calendar-config-keys.h`, `calendar-config.c` und `src/modules/calendar/e-calendar-preferences.c`:
  1. GSchema (`data/org.gnome.Evolution4.gschema.xml`):
     - `calendar-time-divisions` (`i`, default 30): Zeiteinteilung für Tages- und Arbeitswochenansicht (5, 10, 15, 30, 60 Minuten).
     - `calendar-compress-weekend` (`b`, default true): Komprimierung von Samstag und Sonntag in einer gemeinsamen Monatsansichts-Spalte.
     - `calendar-month-scroll-by-week` (`b`, default true): Wochenweises statt monatsweises Blättern beim Scrollen im Monatsraster.
     - `calendar-second-zone` (`s`, default `''`): Zweite Zeitzone für die Tagesansicht.
     - `calendar-second-zones` (`s`, default `''`): Historie der zuletzt verwendeten zweiten Zeitzonen (maximal 5 Einträge, FIFO/MRU-Reihenfolge).
     - `calendar-view`: Offiziell erweitert um `year` und `list` in allen Validierungs-, Migrations- und Einstellungsstufen.
  2. Typisierter Zugriff und Sanitization (`src/services/settings_service.rs`):
     - `sanitize_time_divisions`: Validiert streng auf `5 | 10 | 15 | 30 | 60`, fällt sonst auf Default 30 zurück.
     - `calendar_second_zone` / `calendar_second_zones`: Automatische Aktualisierung der MRU-Liste mit max. 5 Einträgen bei Auswahl einer zweiten Zeitzone.
     - `calendar_view` / `set_calendar_view`: Vollständige Unterstützung von `day`, `week`, `workweek`, `month`, `agenda`, `year` und `list`.
     - `import_setting` und `reset_to_defaults`: Vollständig verdrahtet.
  3. Upstream-Migration (`src/services/migration_service.rs`):
     - Kanonische Abbildung von Upstream-GSettings/Dconf-Schlüsseln: `time-divisions`, `compress-weekend`, `month-scroll-by-week`, `day-second-zone` / `second-zone`, `day-second-zones` / `second-zones`.
     - Dekodierung von GVariant-Array `@as` (`day-second-zones`) in portable kommaseparierte Strings.
     - Strikte Validierung von Zeiteinteilung und Zeitzonennamen mit Warnungen bei ungültigen Werten.
  4. Backup & Restore (`src/services/backup_service.rs`):
     - Alle 5 Einstellungen in `BackupSettings` integriert, serialisiert, deserialisiert und validiert (`calendar_time_divisions` in `[5, 10, 15, 30, 60]`, `calendar_view` inklusive `year` und `list`).
     - Rückwärtskompatibilität für ältere Backups mit expliziten Standardwerten belegt.
  5. UI- und Live-Synchronisation (`src/dialogs/preferences_dialog.rs`, `src/views/calendar_page.rs`):
     - `group_calendar_view`: Erweiterte Standardansichts-Auswahl (inkl. Jahr und Liste), ComboRow für Zeiteinteilung, SwitchRows für Wochenend-Komprimierung und wochenweises Scrollen, EntryRow für zweite Zeitzone.
     - `calendar_page.rs`: `connect_changed` reagiert live auf Änderungen aller Kalenderschlüssel.
  Belegt durch gezielte Unit-Tests in `settings_service::tests`, `migration_service::tests` und `backup_service::tests` sowie 950/950 Workspace-Tests.
- Offener Parent-/Linux-Rest: GNOME Control Center / Desktop-Zeitzonen-Synchronisation und visuelle Dichte/Theming unter Linux.

### GW-SOURCE-SETTINGS

- Status: `BLOCKED_LINUX`; portabler Einstellungs-, Migrations-, Backup- und Pruning-Teil des offenen Elternpakets `P1-CAL-001` / `GW-SOURCE-SELECTOR` geschlossen.
- Portabler Beleg: Upstream-Parität zu `src/libedataserverui/e-source-selector.c` und `src/calendar/gui/calendar-config-keys.h`:
  1. GSchema (`data/org.gnome.Evolution4.gschema.xml`):
     - `task-hidden-sources` (`s`, default `''`): Versteckte Aufgabenlisten-Quellen-UIDs.
     - `memo-hidden-sources` (`s`, default `''`): Versteckte Memolisten-Quellen-UIDs.
     - `addressbook-hidden-sources` (`s`, default `''`): Versteckte Adressbuch-Quellen-UIDs.
     - `calendar-hidden-groups` / `calendar-groups-order` (`s`, default `''`): Versteckte Gruppen und Gruppenreihenfolge für Kalender.
     - `task-hidden-groups` / `task-groups-order` (`s`, default `''`): Versteckte Gruppen und Gruppenreihenfolge für Aufgaben.
     - `memo-hidden-groups` / `memo-groups-order` (`s`, default `''`): Versteckte Gruppen und Gruppenreihenfolge für Memos.
     - `addressbook-hidden-groups` / `addressbook-groups-order` (`s`, default `''`): Versteckte Gruppen und Gruppenreihenfolge für Adressbücher.
  2. Typisierter Zugriff, Normalisierung und atomares Pruning (`src/services/settings_service.rs`):
     - `sanitize_source_list`: Sortierung und Deduplizierung für Sichtbarkeitslisten.
     - `sanitize_ordered_list`: Reihenfolgeerhaltende Deduplizierung für Gruppenanordnungen.
     - Typisierte Accessors für alle 11 neuen GSettings-Schlüssel.
     - `prune_deleted_source(deleted_uid: &str)`: Atomare Bereinigung aller Default-Quellen (`default_calendar_source_uid`, `default_task_source_uid`, `default_memo_source_uid`, `default_address_book_source_uid`, `task_source_filter`, `memo_source_filter`), aller 4 Hidden-Sources-Listen und aller 8 Gruppen-Listen, wenn eine Quelle gelöscht wird.
     - `import_setting` und `reset_to_defaults`: Vollständig verdrahtet.
  3. Upstream-Migration (`src/services/migration_service.rs`):
     - Kanonische Abbildung von Upstream-GSettings/Dconf-Schlüsseln: `task-hidden-sources`, `memo-hidden-sources`, `addressbook-hidden-sources`, `[SourceSelector] /org/gnome/evolution/{calendar,tasks,memos,addressbook}/hidden-groups` und `groups-order`.
     - Dekodierung von GVariant-Array `@as` in portable kommaseparierte Strings.
     - Reihenfolgeerhaltende Deduplizierung und Validierung auf Steuerzeichen.
  4. Backup & Restore (`src/services/backup_service.rs`):
     - Alle 11 neuen Felder in `BackupSettings` integriert, serialisiert, deserialisiert und validiert (`validate_source_or_group_list` auf Max-Größe 4096, Token-Validität, Steuerzeichenfreiheit, keine Dubletten).
     - Vollständige Abwärtskompatibilität für ältere Backups mit Standardwerten belegt.
  Belegt durch gezielte Unit-Tests (`source_and_group_settings_and_pruning_roundtrip`, `source_and_group_settings_upstream_migration_parities`, `source_and_group_settings_backup_validation_and_roundtrip`) und 953/953 Workspace-Tests.
- Offener Parent-/Linux-Rest: Reale EDS-Source-Gruppen (`ESourceGroup`), dynamische Remote-Capabilities und Provider-Registrierung unter Linux.

### GW-REMINDER-SETTINGS

- Status: `BLOCKED_LINUX`; portabler Einstellungs-, Alarm-Policy-, Filter-, Migrations- und Backup-Teil der offenen Elternpakete `P1-CAL-001` / `P1-TASK-001` / `GW-ALARMS` / `GW-TASK-FILTERS` geschlossen.
- Portabler Beleg: Upstream-Parität zu `calendar-config-keys.h`, `calendar-config.c`, `src/modules/calendar/e-calendar-preferences.c` und `org.gnome.evolution.calendar.gschema.xml.in`:
  1. GSchema (`data/org.gnome.Evolution4.gschema.xml`):
     - `use-default-reminder` (`b`, default `false`): Standard-Erinnerung für neue Termine automatisch anlegen.
     - `default-reminder-interval` (`i`, default `15`): Intervall für Standard-Erinnerung.
     - `default-reminder-units` (`s`, default `'minutes'`): Einheit (`minutes`, `hours`, `days`).
     - `use-ba-reminder` (`b`, default `false`): Erinnerung für Geburtstage und Jahrestage.
     - `ba-reminder-interval` (`i`, default `15`): Intervall für Geburtstage/Jahrestage.
     - `ba-reminder-units` (`s`, default `'minutes'`): Einheit für Geburtstage/Jahrestage.
     - `hide-completed-tasks` (`b`, default `false`): Erledigte Aufgaben ausblenden.
     - `hide-completed-tasks-value` (`i`, default `1`): Frist-Schwellwert (0 = sofort ausblenden).
     - `hide-completed-tasks-units` (`s`, default `'days'`): Einheit für Aufgaben-Frist.
     - `hide-cancelled-tasks` (`b`, default `false`): Aufgaben mit Status CANCELLED ausblenden.
     - `hide-cancelled-events` (`b`, default `false`): Termine mit Status CANCELLED ausblenden.
  2. Typisierter Zugriff und Schwellenwert-Berechnung (`src/services/settings_service.rs`):
     - `ReminderDurationUnit` Enum (`Minutes`, `Hours`, `Days`) mit `from_str`, `as_str`, `to_seconds`, `to_minutes`.
     - `hide_completed_tasks_cutoff_timestamp(now_secs)` berechnet `now - value * unit_seconds` exakt nach Upstream `calendar-config.c::calendar_config_get_hide_completed_tasks_sexp`.
     - Typisierte Accessors für alle 11 neuen GSettings-Schlüssel, Synchronisation von `default_reminder_total_minutes` mit legacy `calendar-default-reminder`.
     - `import_setting` und `reset_to_defaults`: Vollständig verdrahtet.
  3. Upstream-Migration (`src/services/migration_service.rs`):
     - Kanonische Abbildung von Upstream-GSettings/Dconf-Schlüsseln: `default-reminder-interval`, `default-reminder-units`, `use-ba-reminder`, `ba-reminder-interval`, `ba-reminder-units`, `hide-completed-tasks`, `hide-completed-tasks-value`, `hide-completed-tasks-units`, `hide-cancelled-tasks`, `hide-cancelled-events`.
     - Normalisierung von String- und Integer-Einheiten (`0`/`minutes`, `1`/`hours`, `2`/`days`).
  4. Backup & Restore (`src/services/backup_service.rs`):
     - Alle 11 neuen Felder in `BackupSettings` integriert, serialisiert, deserialisiert und validiert (`validate_choice` für Einheiten, `validate_u32_setting` für Intervalle/Werte).
     - Abwärtskompatibilität für ältere Backups mit Standardwerten belegt.
  5. Default Alarm Policy (`src/dialogs/event_dialog.rs`):
     - Standard-Display-Alarm wird bei neuen Terminen nur angelegt, wenn `use_default_reminder` aktiv ist, unter Verwendung von `default_reminder_total_minutes`.
     - Beim Bearbeiten bestehender Termine werden Alarme niemals stillschweigend überschrieben.
  6. Aufgaben- und Kalender-Filterung (`src/views/tasks_page.rs`, `src/views/calendar_page.rs`):
     - `task_matches_filter_with_context`: Berücksichtigt `hide_cancelled_tasks` (außer in `TaskFilter::Cancelled`) und `hide_completed_tasks` mit Frist-Cutoff (außer in `TaskFilter::Completed`). Dynamische GSettings-Reaktivität.
     - `event_matches_calendar_filter_with_options`: Berücksichtigt `hide_cancelled_events`. Dynamische GSettings-Reaktivität.
  7. Einstellungen-UI (`src/dialogs/preferences_dialog.rs`):
     - Adw-Preferences-Gruppen für Standard-Erinnerungen, Geburtstage/Jahrestage, abgesagte Termine und Erledigungsfristen für Aufgaben.
  Belegt durch gezielte Unit-Tests (`reminder_and_task_filter_settings_roundtrip_and_cutoff`, `reminder_and_task_filter_settings_upstream_migration_parities`, `reminder_and_task_filter_settings_backup_validation_and_roundtrip`, `task_hide_cancelled_and_completed_filter_parities`, `calendar_event_hide_cancelled_events_filter_tests`) und 958/958 Workspace-Tests.
- Offener Parent-/Linux-Rest: Native EDS-Kalender- und Alarm-Synchronisation sowie Provider-Alarme unter Linux.

### GW-ADDRESSBOOK-SETTINGS

- Status: `BLOCKED_LINUX`; portabler Einstellungs-, Map-Policy-, Autocomplete-, Formatierungs-, Layout-, Migrations- und Backup-Teil der offenen Elternpakete `P1-CONTACT-001` / `GW-CONTACT-SURFACE` / `GW-CONTACT-EDITOR` geschlossen.
- Portabler Beleg: Upstream-Parität zu `src/addressbook/gui/widgets/eab-config.c`, `src/modules/addressbook/autocompletion-config.c`, `e-preferences-window.c` und `data/org.gnome.evolution.addressbook.gschema.xml.in`:
  1. GSchema (`data/org.gnome.Evolution4.gschema.xml`):
     - Autocompletion-Keys: `contact-completion-minimum-query-length` (`i`, default `3`, range 1..100), `contact-completion-show-address` (`b`, default `false`), `contact-completion-book-sources` (`s`, default `''`).
     - Display- &amp; Map-Keys: `contact-address-formatting` (`b`, default `false`), `contact-preview-show-maps` (`b`, default `false`), `contact-preview-home-before-work` (`b`, default `false`), `contact-open-map-target` (`s`, default `'openstreetmap'`), `contact-open-map-prefer-local` (`b`, default `true`).
     - Layout- &amp; Pane-Keys: `contact-layout` (`s`, default `'classic'`), `contact-preview-visible` (`b`, default `true`), `contact-hpane-position` (`i`, default `200`, range 80..4096), `contact-vpane-position` (`i`, default `400`, range 80..4096).
     - 12 Editor-Sektionen (`b`, default `true`): `contact-editor-show-phone`, `contact-editor-show-sip`, `contact-editor-show-im`, `contact-editor-show-home-address`, `contact-editor-show-work-address`, `contact-editor-show-other-address`, `contact-editor-show-web`, `contact-editor-show-job`, `contact-editor-show-dates`, `contact-editor-show-misc`, `contact-editor-show-notes`, `contact-editor-show-certs`.
  2. Typisierter Zugriff und URI-Builder (`src/services/settings_service.rs`):
     - Typisierte Accessors für alle 24 neuen Schlüssel.
     - `build_contact_map_uri(address, target, prefer_local)` erzeugt `geo:` oder OSM/Google-Web-URI mit Fallback nach Upstream-Parität.
     - Integration in `apply_migrated_setting` und `reset_to_defaults`.
  3. Upstream-Migration (`src/services/migration_service.rs`):
     - Kanonische Aliase für `org.gnome.evolution.addressbook` Keys.
     - Normalisierung von Layout (0 -> "classic", 1 -> "vertical"), Map-Target ("openstreetmap" / "google"), Min-Query-Length (1..100) und Pane-Positionen (80..4096).
  4. Backup &amp; Restore (`src/services/backup_service.rs`):
     - Alle 24 Felder in `BackupSettings` integriert, serialisiert, deserialisiert und strikt validiert (`validate_choice`, `validate_u32_setting`, `validate_i32_setting`).
     - Abwärtskompatibilität für ältere Backups mit Standardwerten belegt.
  5. Detailvorschau &amp; Karten-Launcher (`src/views/contacts_page.rs`):
     - `ordered_contact_addresses`: Berücksichtigt `contact-preview-home-before-work` und ordnet `ADR;TYPE=HOME` vor `WORK` ein.
     - `contact_detail_summary_with_options`: Formatiert Adressen nach `contact-address-formatting` und zeigt Kartenstatus nach `contact-preview-show-maps`.
     - `map_btn`: Öffnet `build_contact_map_uri` mit primärer/Fallback-URI; Tooltip und Accessible-Label passen sich dem Ziel an.
     - Live-GSettings-Listener für `contact-open-map-target`, `contact-preview-visible`, `contact-preview-home-before-work`, `contact-preview-show-maps` und `contact-address-formatting`.
  6. Einstellungen-UI (`src/dialogs/preferences_dialog.rs`):
     - Drei neue Preferences-Gruppen in der Kontakt-Seite: "Darstellung &amp; Karten" (SwitchRows, ComboRow für Map-Target), "Autovervollständigung" (SwitchRow, SpinRow) und "Kontakteditor-Abschnitte" (12 SwitchRows).
  Belegt durch gezielte Unit-Tests (`contact_settings_roundtrip_defaults_and_map_uri`, `addressbook_settings_upstream_migration_parities`, `contact_settings_backup_validation_and_roundtrip`, `ordered_contact_addresses_respects_home_before_work`, `contact_detail_summary_with_options_formats_maps_and_lines`) und 963/963 Workspace-Tests.
- Offener Parent-/Linux-Rest: EBook/EDS source enumeration, native geo: desktop application integration und Linux-Visuals.


### GW-TASK-EDITOR-LIFECYCLE

- Status: `BLOCKED_LINUX`; portabler Teil des offenen Elternpakets
  `GW-TASK-EDITOR` geschlossen.
- Portabler Beleg: Fremdfeldänderungen erhalten `STATUS`, `PERCENT-COMPLETE` und
  `COMPLETED` unabhängig einschließlich fehlender oder nicht darstellbarer Werte;
  nur echte Lifecycle-Control-Interaktion normalisiert die gekoppelte Tripel-
  Semantik. Attachments, unknown properties und geordnete VALARMs bleiben erhalten.
- Offener Parent-/Linux-Rest: Delegation/Teilnehmer, native Task-Capabilities,
  Provider-Konflikte und vollständige visuelle Editor-Abnahme.

### GW-TASK-ACTIONS-BATCH

- Status: `BLOCKED_LINUX`; portabler Auswahl-/Save-as-Teil des offenen
  Elternpakets `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: echte 0/1/n-Auswahl anhand Source+UID, Aktualisierung nach
  Filter/Refresh, selection-sensitive Controls und vor dem FileDialog vollständig
  validierter gemeinsamer VCALENDAR-Payload mit exakt einem VTODO je Auswahl;
  das Ziel wird atomar ersetzt. Stale, doppelte oder ungültige Kandidaten brechen
  ohne Datei- oder Taskmutation ab.
- Offener Parent-/Linux-Rest: Delegate, Print, echte iTIP-Zustellung und
  URL-Handler-Abnahme, weitere capability-gesteuerte Batchmutationen, EDS-ACLs sowie
  Desktop-/File-Portal-Abnahme.

### GW-TASK-ACTIONS-FORWARD-URL

- Status: `BLOCKED_LINUX`; portabler Action-Teil des offenen Elternpakets
  `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: Forward und Open URL sind für exakt eine aktuelle Source+UID-
  Auswahl sensitiv. Forward validiert genau einen passenden VTODO, entfernt
  VALARMs wie upstream und stellt ohne Quellmutation einen deterministischen
  `METHOD:PUBLISH`-Payload bereit. Open URL liest die URL aus demselben exakten
  VTODO und lässt ausschließlich die gehärtete gemeinsame HTTP(S)-Policy zum
  Desktop-Handler durch; stale, fehlende, UID-fremde und unsichere Werte brechen ab.
- Offener Parent-/Linux-Rest: echte Composer-/iTIP-Zustellung statt portabler
  Payload-Bereitstellung, Desktop-Handler-Abnahme, Delegate,
  EDS-Capabilities und Providerwirkung; native Printwirkung führt der separate
  Print-Slice als Linux-Rest.

### GW-TASK-ACTIONS-PRINT

- Status: `BLOCKED_LINUX`; portabler Print-Preparation-Teil des offenen
  Elternpakets `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: Print ist nur für exakt eine aktuelle Source+UID-Auswahl
  sensitiv, validiert denselben eindeutigen VTODO und erzeugt daraus ein
  deterministisches unveränderliches HTML-/Text-Dokument. Alle Felder sind
  escaped, aktive/remote Inhalte fehlen, und Preparing/Result/Error benennen
  explizit den noch ausstehenden GTK-Druckdialog statt einen Jobabschluss zu melden.
- Offener Parent-/Linux-Rest: nativer GTK-Druckdialog/Portal, Preview,
  Seitenlayout/Jobabschluss, EDS-Zeitzone/Locale und visuelle Linux-Abnahme.

### GW-TASK-ACTIONS-LIFECYCLE-BATCH

- Status: `BLOCKED_LINUX`; portabler Complete-/Incomplete-Teil des offenen
  Elternpakets `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: Actions spiegeln upstreams 0/1/n-/mixed-state-Sensitivity,
  lösen jede Source+UID genau einmal auf und verlangen vor dem ersten Write eine
  vollständig schreibfähige Auswahl. Complete setzt STATUS/100%, bewahrt vorhandene
  COMPLETED-Werte und verwendet für fehlende Werte einen Batch-Zeitpunkt;
  Incomplete setzt NEEDS-ACTION und entfernt COMPLETED/PERCENT-COMPLETE. Die
  sequentiellen upstream-nahen ECal-Writes laufen außerhalb GTK; nur UID-validierte
  autoritative Erfolge werden angewendet, Partial-/Channel-Fehler bleiben sichtbar.
- Offener Parent-/Linux-Rest: bestätigte externe EDS-Writability-Capabilities,
  reale Provider-Teilfehler/Monitor-Reconciliation, Konflikte, Offline und visuelle
  Linux-Abnahme; Delegate/iTIP-Delivery und native Delete-Sonderfälle bleiben separate
  Parent-Reste.

### GW-TASK-ACTIONS-DELETE-BATCH

- Status: `BLOCKED_LINUX`; portabler Delete-selection-Teil des offenen
  Elternpakets `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: 0/1/n-Sensitivity verlangt exakte UID-passende VTODOs und
  ausschließlich bestätigte writable Sources. Eine named/count-aware destructive
  Confirmation wird vor Dispatch erneut gegen Auswahl und Capabilities geprüft;
  Cancel mutiert nichts. Der Worker löscht wie upstream sequentiell und stoppt
  beim ersten Fehler. Nur der bestätigte Prefix wird entfernt; Failure und
  unattempted Rest bleiben sichtbar. Toolbar, Row und Context teilen diesen Pfad.
- Offener Parent-/Linux-Rest: RECURRENCE-ID/MOD_THIS, Single-Assign-iTIP-
  Retract/Cancel-Notice, externe EDS-Writability, Provider-Teilfehler/Monitor-
  Reconciliation, Offline und visuelle Linux-Abnahme.

### GW-TASK-ACTIONS-CLIPBOARD

- Status: `BLOCKED_LINUX`; portabler Clipboard-/Select-All-Teil des offenen
  Elternpakets `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: Copy und Cut validieren die vollständige sichtbare
  Source+UID-Auswahl und erzeugen einen gemeinsamen VTODO-VCALENDAR-Payload;
  Cut kopiert vor dem zentral bestätigten Delete-Pfad. Paste akzeptiert bare
  VTODOs oder VCALENDAR, revalidiert die schreibfähige Standardquelle nach dem
  asynchronen Clipboard-Read und im Worker, vergibt pro Komponente eine neue UID,
  schreibt sequentiell und wendet nur den autoritativ bestätigten Prefix an.
  Channel-/Apply-Unsicherheit lädt den Bestand neu. Ctrl+C/X/V/A greift nur bei
  Tasklisten-Fokus; Select All betrifft die sichtbaren Zeilen.
- Offener Parent-/Linux-Rest: VTIMEZONE-Provisionierung für TZID-Payloads,
  externe EDS-Writability/ACLs, Provider-Teilfehler/Monitor-Reconciliation,
  native Clipboard-Targets und visuelle Linux-Abnahme.

### GW-TASK-ACTIONS-BULK-EDIT

- Status: `BLOCKED_LINUX`; portabler Mehrfachbearbeitungs-Teil des offenen
  Elternpakets `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: Bulk Edit bleibt wie upstream für jede nichtleere Auswahl
  sensitiv und ist bei Tasklisten-Fokus über Ctrl+B erreichbar; Aktivierung und
  Dispatch prüfen alle aktuellen Source+UID-Identitäten und writable Sources.
  DTSTART, DUE, COMPLETED, STATUS, PRIORITY, PERCENT-COMPLETE, CLASS,
  ESTIMATED-DURATION sowie Kategorie-Add/Remove sind einzeln opt-in. Datums- und
  Lifecycle-Felder folgen den upstream-Kopplungen, escaped Kategoriekommas sowie
  unbekannte Properties, Recurrence, Attachments und VALARMs bleiben erhalten.
  Der Worker schreibt sequentiell, stoppt beim ersten Fehler, baut bestätigte
  Zeilen aus autoritativen Komponenten neu auf und lädt bei unsicherem Ergebnis.
- Offener Parent-/Linux-Rest: VTIMEZONE-/TZID-Transformation, native EDS-
  Capabilities/ACLs, Provider-Teilfehler/Monitor-Reconciliation, Offline und
  visuelle Linux-Abnahme.

### GW-TASK-ACTIONS-FIND-PREVIEW

- Status: `BLOCKED_LINUX`; portabler Find-/Preview-Teil des offenen
  Elternpakets `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: Genau eine aktuelle Source+UID-Auswahl rendert die sichere,
  GTK-native VTODO-Vorschau; null/mehrfach/stale leert sie. Ctrl+M schaltet die
  persistierte Pane; Classic liegt unter und Vertical neben der Liste, mit je
  eigener persistierter Divider-Position. Das
  upstream-kompatible Ctrl+Shift+F öffnet nur bei Tasklisten-Fokus und gültiger
  Einzelauswahl eine Unicode-offset-sichere Vor-/Zurück-Suche mit Wrap und
  invalidiert Query/Highlights bei Auswahl-, Komponenten- oder Sichtwechsel.
  Upstream-`show-task-preview`/`task-layout` migrieren strikt in die neuen Keys.
- Offener Parent-/Linux-Rest: aktive HTML-Darstellung, Attachment-Aktionen,
  native EDS-Komponentenformatierung, Fokus/IME und visuelle Linux-Abnahme.

### GW-TASK-ACTIONS-PURGE

- Status: `BLOCKED_LINUX`; portabler Purge-Teil des offenen Elternpakets
  `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: Ctrl+E ist auswahlunabhängig, plant alle erledigten Aufgaben
  aus geladenen schreibfähigen Quellen, überspringt Read-only-Quellen, bestätigt
  den exakten Count und revalidiert ihn vor Dispatch. Der Worker löscht
  sequentiell, stoppt beim ersten Fehler, wendet nur den bestätigten Prefix an
  und lädt bei verlorenem Ergebnis den autoritativen Snapshot neu.
- Offener Parent-/Linux-Rest: RECURRENCE-ID/MOD_THIS, externe EDS-Writability,
  Provider-/Monitor-Reconciliation, Offline und visuelle Linux-Abnahme.

### GW-TASK-ACTIONS-LIST-PRINT

- Status: `BLOCKED_LINUX`; portabler Listen-Print-/Preview-Teil des offenen
  Elternpakets `GW-TASK-SURFACE` geschlossen.
- Portabler Beleg: Print und Print Preview verwenden selection-unabhängig einen
  unveränderlichen Snapshot der tatsächlich sichtbaren, gefilterten und
  sortierten Taskzeilen. Jede Source+UID-Identität wird exakt einmal gegen den
  aktuellen VTODO-Bestand validiert; empty/duplicate/stale/malformed bricht
  sichtbar ab. HTML und Text sind deterministisch, alle HTML-Zellen escaped und
  frei von aktiven/remote Inhalten. Ctrl+P greift nur bei Tasklisten-Fokus und
  bleibt vom separaten Single-Task-Print getrennt.
- Offener Parent-/Linux-Rest: nativer GTK-Print-/Preview-Dialog, Portal,
  Seitenlayout, Jobabschluss, Locale/Zeitzone und visuelle Linux-Abnahme.

### GW-TASK-LIST-REFRESH

- Status: `BLOCKED_LINUX`; der portable/native Contract für Upstreams
  `task-list-refresh` ist geschlossen, die reale Providerabnahme bleibt offen.
- Portabler Beleg: Nur eine verfügbare primäre Aufgabenliste kann den Worker
  starten; „Alle Aufgabenlisten“, fehlende Quellen, Parallelmutationen und
  bekannte Unsupported-Quellen bleiben gesperrt. Der Linux-Adapter löst die
  exakte ESource-UID auf, verlangt die TASK_LIST-Extension, verbindet ECalClient,
  prüft `e_client_check_refresh_supported` und ruft erst dann den synchronen
  Refresh. Erfolg wird erst nach einem autoritativen Gesamtreload übernommen;
  Generation und primäre Source-UID verwerfen verspätete Resultate, während die
  sichtbare Source+UID-Auswahl beim Neuaufbau erhalten oder korrekt bereinigt
  wird. Lokal wird ehrlich nur neu geladen, ohne Provider-Refresh vorzutäuschen.
- Offener Parent-/Linux-Rest: Auth-Prompt/Cancel, Offline/Providerfehler,
  Monitor-Reconciliation und
  visuelle Linux-Abnahme.

### GW-TASK-SOURCE-CAPABILITIES

- Status: `BLOCKED_LINUX`; die frühere generische Calendar-/GOA-Projektion ist
  aus der Aufgabenlisten- und Editor-Grenze entfernt.
- Portabler Beleg: Der native Adapter enumeriert ausschließlich aktivierte
  Quellen mit der zum Komponententyp passenden Extension, für Aufgaben also
  `E_SOURCE_EXTENSION_TASK_LIST`. Jede exakte UID behält Anzeigename,
  Connection-/Availability-, Readonly/Writable- und Refresh-Support-Zustand.
  Verbindungsfehler behalten die Quelle unavailable in der Liste und liefern
  eine sichtbare Partial-Warnung, statt fremde Kalenderkonten als Tasklisten zu
  erfinden. Hauptliste, Editor, Quick Edit, Toggle, Row-Aktivierung,
  Batch-Sensitivity und Refresh verwenden dieselbe fail-closed Projektion;
  lokale portable Tasks bleiben ein expliziter separater Source-Typ.
- Offener Parent-/Linux-Rest: reale Registry mit gemischten Calendar/Task/Memo-
  Extensions, dynamische Capability-/ACL-Änderung nach Start, Provider-Auth,
  Offline/Partial-Connect und visuelle Linux-Abnahme.

### GW-MEMO-SOURCE-CAPABILITIES

- Status: `BLOCKED_LINUX`; der Memo-Surface-/Editor-Rest verwendet jetzt die
  wiederverwendbare exakte ECal-Discovery statt generischer Kalenderkonten.
- Portabler Beleg: Hauptlistenfilter und Editorziel werden im Worker nur aus
  `E_SOURCE_EXTENSION_MEMO_LIST`-Quellen aufgebaut. Stabile UID/Anzeige und
  connected/available/readonly-writable werden getrennt projiziert; blanke und
  doppelte UIDs werden verworfen, offline und read-only mutieren fail-closed.
  Ein persistierter Filter oder die Quelle einer geöffneten Notiz bleibt bei
  Ausfall sichtbar unavailable. `local-calendar` erscheint nur bei bestätigtem
  schreibfähigem lokalen Store, und ein nach Dialogschluss eintreffendes
  Discovery-Ergebnis wird verworfen.
- Offener Parent-/Linux-Rest: reale Memo-Provider-/ACL-Wirkung, dynamische
  Capability-Änderung, Auth/Offline/Partial-Connect, Provider-Anhänge und
  visuelle Linux-Abnahme.

### GW-TASK-LIST-MANAGEMENT

- Status: `BLOCKED_LINUX`; Upstreams Primary-Task-List-Aktionen Copy, Rename und
  Delete sowie die lokalen New-/Properties-Pfade sind über exakte
  ESource-Identitäten und Metadaten verdrahtet.
- Portabler Beleg: Source-Discovery liefert zusätzlich source-writable,
  client-writable, removable, remote-deletable und collection-owned. Die UI
  verwendet dieselben Gates wie Upstream, hält `ESource:writable` für
  Rename/Properties getrennt von ECalClient-readonly für Taskmutationen,
  sperrt parallele Taskmutationen und verwirft stale Generationen. Rename löst
  die Source erneut auf, verlangt TASK_LIST, aktuelle Source-Writability und
  keine Collection-Anbindung und schreibt erst dann den Namen. Delete
  bestätigt destruktiv und verwendet remote delete ausschließlich bei
  bestätigter Capability, sonst nur removable. Copy wählt eine andere exakte
  TASK_LIST-Quelle, verbindet beide ECalClients, ersetzt identische UIDs,
  kopiert sonst alle referenzierten Zeitzonen vor Create und stoppt beim ersten
  Providerfehler mit exakten bestätigten/aktualisierten Zählern. Erfolg und
  Teilfehler werden autoritativ neu geladen; Source+UID-Auswahl wird erhalten
  oder nach Delete bereinigt. New legt nach strikter Name-/`#RRGGBB`-Prüfung
  eine exakte lokale TASK_LIST unter `local-stub` an, setzt Backend, Farbe und
  Writability, committed über die EDS-Registry und unterscheidet bestätigte
  Verfügbarkeit von einem erfolgreichen Commit mit noch nicht erreichbarem
  Client. Optional wird erst die bestätigte UID als Default gespeichert.
  Properties lädt vor Anzeige die exakte lokale Source, bearbeitet Name,
  Farbe, Default, optionale ICS-Datei samt Update-Erlaubnis und E-Mail, sperrt
  die Dateiauswahl für die eingebaute Quelle und committed eine scratch Source;
  Commit-Erfolg und nachgelagerte Client-Verfügbarkeit bleiben getrennt.
- Offener Parent-/Linux-Rest: Upstreams nicht-lokale, provider-spezifische
  `New Task List` und `Task List Properties` beruhen auf Evolution-internen
  `ECalSourceConfig`-Backends (Local/CalDAV/WebDAV/Collection usw.), die im
  GTK4-Port noch nicht vorhanden sind. Außerdem bleiben C-Compile,
  Registry-/Auth-/Offline-/Providerwirkung, dynamische Capability-Änderungen
  und visuelle Linux-Abnahme offen.

### GW-TASK-ACTIONS-ASSIGN

- Status: `BLOCKED_LINUX`; portabler Assign-Metadaten-/Handoff-Slice ist
  geschlossen, echte iTIP-Zustellung bleibt offen.
- Portabler Beleg: EDS-Discovery projiziert `NO_TASK_ASSIGNMENT` und
  `NO_CONV_TO_ASSIGN_TASK` unabhängig von Source-/Client-Writability. Die UI
  aktiviert Assign nur für exakt eine aktuelle Source+UID-Auswahl auf einem
  connected, client-writable Provider, der beide Operationen unterstützt, und
  revalidiert Auswahl, Komponente und Capabilities vor der Mutation.
  ORGANIZER/ATTENDEE akzeptieren sichere E-Mail-Kurzformen oder vollständige
  iCalendar-Zeilen, verlangen Organizer plus mindestens eine Person, deduplizieren
  identische Teilnehmende und bewahren fremde sowie nested Properties. Der
  Worker speichert über exact Source+UID, verlangt anschließend einen
  autoritativen EDS-Reload und stellt erst dann einen `METHOD:REQUEST`-Payload
  bereit; stale Generationen und Reload-Unsicherheit werden sichtbar verworfen.
- Offener Parent-/Linux-Rest: echtes iTIP-Senden/Composer-Handoff,
  Organizer-Identitäten, Antworten, Delegate-/Delegated-From-/To-Ketten,
  Provider-ACLs/Servernormalisierung, Delete-Retract und Linux-Visuals.

### MAIL-FOLLOW-UP-DUE

- Status: `BLOCKED_LINUX`; der portable Reader-Aktionsslice ist geschlossen.
- Portabler Beleg: vollständige Follow-up-Metadaten aus State/Text/Due-by/
  Completed-on durch Model, versioniertes Sidecar, Command-History und Camel-
  Parser; expliziter selection-sensitive Editor samt Message-Menü, Clear und
  Complete; exakte Undo-/Redo-Inverse. Graph-Due und nicht schreibbarer nativer
  Custom-Text werden ohne Mutation abgelehnt statt still verworfen.
- Linux-Rest: reale EDS/Camel-Tag-Roundtrips, Graph-/Camel-Providerwirkung,
  Locale/Zeitzonen- und visuelle Dialogabnahme.

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
  des EDS-Watchers `BLOCKED_LINUX`; `GW-EVENT-EDITOR` schließt als portabler
  Schnitt das verlustfreie Event-Editing (`update_existing_event_component`) mit
  vollständigem Erhalt von RFC5545-Top-Level-/Vendor-Properties (ORGANIZER, GEO,
  URL, PRIORITY, X-EVOLUTION-*), Parameter-Erhalt bei Teilnehmern und Anhängen,
  Ganztags-/Uhrzeit-Moduswechsel (`VALUE=DATE`), Multi-VALARM-Synchronisation
  und Eingabeschutz ab und ist `BLOCKED_LINUX`; `GW-MEMO-EDITOR` ergänzt als
  eigener atomarer Schnitt mit exakt diesem Produktionsscope in
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
  sicher begrenzter Dublettenpfad. Die portablen Slices `GW-CONTACT-LISTS` (vCard-3/4-
  Mitglieder und Member-UI), `GW-CONTACT-ACTIONS-BATCH-EXPORT` (0/1/n-Auswahl, Batch-Mail,
  atomarer vCard-Export), `GW-CONTACT-ACTIONS-DELETE-BATCH` (all-writable Preflight, named/count
  Confirmation, sequentielles Stop-on-first-error-Delete), `GW-CONTACT-ACTIONS-PRINT`
  (Kartendruck mit Alphabet-Headings, Adresslistenauflösung, Tabellendruck sichtbarer Kontakte,
  `<Ctrl>P`), `GW-CONTACT-ACTIONS-TRANSFER` (Kopieren/Verschieben zwischen Adressbüchern,
  Create-before-delete, Fresh-UIDs, AlertDialog mit Dropdown) und `GW-CONTACT-SEARCH`
  (typisierter Search-AST, Feld-/Operator-Registry, ContactSearchDialog, Quick-Search,
  Outer-Filters, Treffer-/Preview-Sync, Worker-Generierungen) sind portabel implementiert
  und stehen auf `BLOCKED_LINUX`. Weitere Kontaktflächen (z. B. Contact Surface Map-Linking) sowie
  Linux-EBook/GAL-/ACL-Abnahme bleiben offen.

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
- Neu atomisiert und portabel geschlossen sind `MAIL-ACCELERATOR-CONTRACT`,
  `GW-CAL-ACTIONS-TRANSFER-DELETE`, `GW-CONTACT-ACTIONS-BATCH-EXPORT` und
  `GW-TASK-EDITOR-LIFECYCLE`, `GW-TASK-ACTIONS-BATCH` sowie
  `GW-TASK-ACTIONS-FORWARD-URL`, `GW-TASK-ACTIONS-PRINT` und
  `GW-TASK-ACTIONS-LIFECYCLE-BATCH`, `GW-TASK-ACTIONS-DELETE-BATCH`,
  `GW-TASK-ACTIONS-CLIPBOARD`, `GW-TASK-ACTIONS-BULK-EDIT`,
  `GW-TASK-ACTIONS-FIND-PREVIEW`, `GW-TASK-ACTIONS-PURGE`,
  `GW-TASK-ACTIONS-LIST-PRINT`, `GW-TASK-LIST-REFRESH`,
  `GW-TASK-SOURCE-CAPABILITIES`, `GW-MEMO-SOURCE-CAPABILITIES` sowie
  `MAIL-FOLLOW-UP-DUE`; ihre dokumentierten
  Parent-/Linux-Reste bleiben offen und verhindern eine Vollparitätsbehauptung.
- Portable Evidenz: `git diff --check`, gezieltes Rustfmt, strikte
  Schema-Prüfung, normaler `cargo check --workspace --locked` und
  `RUSTFLAGS='--cfg=eds_camel_bridge' cargo check --workspace --locked` sind
  grün. `cargo test --workspace --locked --no-fail-fast --quiet` ist mit
  835/835 Tests grün; der erzwungene Camel-Service-Lauf ist 17/17 grün. Der
  globale Format-Check bleibt wegen vorbestehender Drift offen. Das ist keine
  Linux-Abnahme.
- Keine weitere portable Implementierungspriorität ist derzeit atomar `READY`;
  die achtzehn neuen Teilpakete schließen nur ihren benannten Scope. Vor der nächsten
  Priorisierung muss der stärkste verbleibende Parent-Rest frisch atomisiert und
  mit Schreibbereich, Abhängigkeiten, Positiv-/Negativpfad und Linux-Grenze belegt
  werden.
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
