# Linux-/EDS-Roadmap

Diese Datei enthält ausschließlich Arbeiten, die eine Linux-Umgebung mit
GTK4, libadwaita und dem originalen Evolution Data Server (EDS) benötigen.
Sie ergänzt die allgemeine [ROADMAP.md](ROADMAP.md). Die priorisierte,
atomare Reihenfolge steht in [ROADMAP_EXECUTION.md](ROADMAP_EXECUTION.md);
plattformunabhängige Rust-, GTK4-rs- und Service-Arbeiten bleiben dort.
Die statischen Quellen-/UI-/Settings-Belege stehen in
[`docs/UPSTREAM_GROUPWARE_PARITY_AUDIT.md`](docs/UPSTREAM_GROUPWARE_PARITY_AUDIT.md),
[`docs/UPSTREAM_MAIL_SHELL_PARITY_AUDIT.md`](docs/UPSTREAM_MAIL_SHELL_PARITY_AUDIT.md)
und [`docs/UPSTREAM_SETTINGS_PARITY_AUDIT.md`](docs/UPSTREAM_SETTINGS_PARITY_AUDIT.md);
deren portable Pakete ersetzen keine Linux-Abnahme.

Ein Punkt ist erst `[x]`, wenn er auf Linux reproduzierbar gebaut und mit
einem echten EDS-/GNOME-Konto ausgeführt wurde. Ein bestandener Rust-Test auf
macOS oder ein vorhandener UI-Button zählt für diese Datei nicht als Beleg.

Status:

- `[x]` = Linux-Build, Laufzeit, Fehlerpfad und relevante Tests verifiziert
- `[~]` = Adapter oder Testvorbereitung vorhanden, aber Linux-Laufzeitprüfung
  oder ein Teil des Abnahmekriteriums fehlt
- `[ ]` = noch offen

## LLM-Ausführung und P0-Handoffs

Ein Agent auf macOS darf diese Punkte vorbereiten, aber nicht als bestanden
markieren. Ein Linux-Agent beginnt mit dem ersten offenen P0-Handoff, dessen
zugehöriger portabler Punkt vorbereitet ist. Vor jedem Lauf sind Commit,
Distribution, Bibliotheksversionen und Kontotyp zu protokollieren.

Der aktuelle uncommitted Arbeitsstand umfasst `MAIL-TRANSPORT-COMMANDS`,
`SET-01`, den eigenen atomaren `GW-MEMO-EDITOR` und `GW-REMINDER-RUNTIME`;
alle vier portablen Implementierungen sind unabhängig reviewt und stehen wegen
des nativen/provider-/Desktop-Rests auf `BLOCKED_LINUX`. Der
`P0-MAIL-002A-WORKER-SETTINGS`-Handoff ist portabel geschlossen; das schließt
weder `P0-MAIL-002A` noch Linux-, Visual- oder Großpostfach-Abnahme. Cargo-
Format-/Check-, strikte Schema- und Diff-Prüfung, normaler und
`eds_camel_bridge`-Cargo-Check sind grün; der finale unsandboxed
portable/macOS-Workspace-Testlauf hat 564/564 Tests bestanden; das ist keine
Linux-Abnahme.
Ein früherer sandboxed Lauf konnte 18 Graph-Loopback-Mockserver nicht binden
und ist nicht der finale Testbeleg. Das ist nur eine portable Vorbedingung.
Kein Audit-Slice, kein visueller Befund und kein EDS-/Provider-Rest wird deshalb
als `[x]` geführt.

### Audit-abgeleitete Linux-Handoffs

- `[ ]` `MAIL-PRINT-ACTION`: Liste und Reader drucken, Vorschau/PDF und
  `Ctrl+P` prüfen; GtkPrint/Portal-Fehler, Abbruch, HTML/WebKitGTK-
  Seitenumbrüche und visuelle Übereinstimmung dokumentieren. Der portable
  Snapshot-/Job-/Cancel-Vertrag ist abgeschlossen und nicht erneut zu bauen.
- `[ ]` `MAIL-TRANSPORT-COMMANDS`: Die portablen Action-/Statusgates sind
  abgeschlossen und unabhängig reviewt. Unter Linux Send/Receive/All/Stop und
  Offline mit `F12`, echter Camel-/EDS-Authentifizierung, provider-bestätigter
  Queue, Live-Fortschritt, Abbruch und Providerfehlern prüfen.
  `win.refresh-mail`/`F5` darf die getrennten Richtungen nicht wieder als eine
  Aktion ausgeben.
- `[ ]` `COMPOSER-ACTION-SURFACE`: Menü-/Toolbar-/Shortcutpfade für Send,
  Draft, Save-as, Print, Close/New, Receipt, Priority, Charset, Sign/Encrypt
  sowie Recovery gegen EDS-Identität, Drafts/Sent, WebKitGTK, Portal und
  Crypto prüfen. Dirty-/atomare Save-/MIME-Verträge sind portabel belegt.
- `[ ]` `GW-ALARMS`: den portablen Multi-VALARM-Parser/Editor/Scheduler gegen
  echte ECal-VEVENT/VTODO/VJOURNAL-Quellen, ClientView-Watcher, Sourcewechsel,
  Factory-Neustart, absolute/relative Trigger, `RELATED`, Repeat/Duration und
  Zeitzone/DST abnehmen. AUDIO/EMAIL/PROCEDURE/Unknown dürfen auch auf Linux
  nie vom portablen Scheduler ausgeführt werden.
- `[ ]` `GW-MEMO-EDITOR`: Der portable Produktionsschnitt umfasst exakt
  `src/services/calendar_store_service.rs` und `src/views/memos_page.rs`;
  der separate test-only Cross-Surface-RFC5545-Test liegt in
  `src/views/tasks_page.rs`. Der Schnitt ist abgeschlossen und unabhängig
  reviewt. Unter Linux EDS/VJOURNAL-Capabilities,
  ACLs, Provider-Attachment-Transfer, Change-Monitor/Factory-Restart sowie
  Open-URL-/Print-/Desktop-Aktionen prüfen; Save/Discard/Cancel, stale/confirmed
  mutation, Export und der 75-Oktett-/unfolded-ATTACH-Vertrag werden nicht
  erneut portabel gebaut.
- `[ ]` `GW-REMINDER-RUNTIME`: der portable Runtime-Schnitt ist implementiert
  und unabhängig reviewt, aber nicht Linux-abgenommen. Unter GNOME/Wayland
  mit echten EDS-Quellen `EReminderWatcher` und EDS-`ClientView` anschließen;
  Event/Task/Memo-Source-/UID-Routing, providerseitige Alarme, Notification-
  Actions, Fensterfokus und die Wiederverwendung des Reminder-Fensters prüfen.
  Zusätzlich Sleep/Wakeup, Offline-Wiederaufnahme, EDS-Factory-Neustart,
  doppelte/verspätete Source-Signale und den systemweiten Notification-Daemon
  testen. Erwartet werden genau-einmalige Zustellung, gültige Open/10m/1h/
  Dismiss-Aktionen, sichtbare Fehler bei fehlender Quelle/Komponente und kein
  stale Öffnen nach einer neueren Generation; der portable Teststand ersetzt
  diese Linux-Abnahme nicht.
- `[ ]` `SET-01`: Der portable Backup-/Migrationsschnitt ist abgeschlossen und
  unabhängig reviewt. Unter Linux Restore-/Preferences-Wirkung, native
  GSettings-Werte und bewusste EDS/Camel/GOA-/Secret-Service-Ausschlüsse
  sichtbar abnehmen.
- `[ ]` `GW-TASK-FILTERS`: EDS-S-Expressions/Rangegrenzen, Source-Refresh,
  Zeitzone/DST, Query-Fehler und die sichtbare Trennung lokaler Filter von
  Provider-Suche gegen reale Aufgabenquellen prüfen.
- `[ ]` `GW-CONTACT-LISTS`: vCard 3/4-Listen, lokale/externe/verschachtelte
  Mitglieder und ungültige Member gegen EBook-Serverfähigkeiten, ACLs,
  Member-UIDs, parallele Änderungen und Providerfehler prüfen; atomare
  portable Speicherung ist kein Beleg für native Listenparität.

### LINUX-P0-MAIL-001 — Konto, Ordner, Nachricht und WebKit-Reader

- Verknüpft: `P0-MAIL-001` und `P0-SYNC-001` aus
  `ROADMAP_EXECUTION.md`.
- Portable Basis: Historische Commits `10b0321` (MIME-Parser/HTML-Sanitizer)
  und `d6e7dcc`
  (Viewer-Komposition, Fallbackzustände und Remote-Freigabe) sowie der
  `P0-SYNC-001A`-Stand mit begrenzten Page-Windows, opaken Graph-NextLink-
  Cursorn, UID-Deduplizierung, generationensicheren Modellen und normalisierten
  Camel-Events. Der aktuelle uncommitted portable Verification-Snapshot umfasst
  zusätzlich den einmal vollständigen 564/564-Testlauf; keine dieser Angaben
  ersetzt die folgende Linux-Abnahme.
- Vorbereitung: vollständiger portabler Testlauf grün; ein EDS-Testkonto mit
  bekannten Plaintext-, HTML-, CID-, Remote-Bild- und Anhangsnachrichten.
- Ablauf:
  1. App kalt starten und Reaktionsfähigkeit während Quellen-/Ordnerladen
     prüfen.
  2. Konto und verschachtelte Ordner mit Evolution vergleichen.
  3. Jede bekannte Nachricht öffnen, zwischen ihnen wechseln und Paging sowie
     Refresh auslösen.
  4. Remote-Bilder zunächst blockiert und danach explizit freigegeben prüfen.
  5. Anhänge speichern; zusätzlich Abbruch und nicht beschreibbares Ziel
     testen.
  6. Für eine Camel- und eine Graph-Nachricht `Ctrl+U` sowie den View- und
     Kontextmenüpfad öffnen. Die Quelle byteweise mit einer unabhängig
     gespeicherten `.eml` vergleichen; Offline-/fehlende-Nachricht-Fehler
     müssen eine klar gekennzeichnete Rekonstruktion zeigen und dürfen GTK
     nicht blockieren. Dieselbe Nachricht zweimal öffnen: Das vorhandene Fenster
     muss fokussiert statt dupliziert werden. Einen langsamen Abruf schließen
     und dieselbe Nachricht sofort erneut öffnen; das alte Ergebnis darf weder
     erscheinen noch den neuen Lade-/Fehlerzustand ersetzen.
  7. „Alle Nachrichtenkopfzeilen anzeigen“ aktivieren, zwischen Nachrichten
     wechseln, die Ansicht deaktivieren und nach einem Neustart erneut prüfen.
     Originale Faltungen/Mehrfachheader müssen erhalten bleiben; die maximal
     200 Pixel hohe formatierte Kopfzeilenfläche darf den Reader nicht
     verdrängen und darf nicht wie die rohe Quellansicht aussehen.
  8. Unter Einstellungen die normale Kopfzeilenliste mit einer eigenen
     `X-Project`-Zeile ergänzen, ein Standardfeld deaktivieren, zwei Felder
     verschieben und die App neu starten. Reihenfolge und Sichtbarkeit müssen
     im offenen Reader sofort und nach Neustart identisch sein. Danach Backup
     erstellen, Defaults zurücksetzen und wiederherstellen. Gegen je eine
     Camel- und Graph-Nachricht Werte und Reihenfolge einschließlich zweier
     gleichnamiger Header vergleichen; `Face` muss wie in Evolution visuell
     behandelt werden und darf nicht als lange Base64-Zeile erscheinen.
- Erwartung: kein leerer Reader, kein ungefragter Remote-Request, korrekte
  Hintergründe/CSS, stabile Auswahl, keine Duplikate und kein GTK-Hänger.
- Zusätzlich prüfen: Bei schnellem A→B→C-Umschalten mit großen CID-Anlagen darf
  nur C sichtbar werden; CPU/RSS dürfen nicht durch wartende Render-Threads oder
  wiederholte Anlagenkopien anwachsen. Kontozeilen haben kein dekoratives Icon,
  optionale Mailicons zeigen entweder das Theme-Symbol oder den Fallback.
- Evidenz: Logmarker, inhaltsneutrale Screenshots, Testkorpus-IDs und Commit.

### LINUX-P0-MAIL-003 — Reader-Churn, Icons, Dichte und Unread-Counter

- Verknüpft: `P0-MAIL-001D`, `P0-MAIL-002A` und `P0-MAIL-002B` aus
  `ROADMAP_EXECUTION.md`.
- Status: `[ ]`; dieser Punkt darf weder aus einem macOS-Build noch aus einem
  einzelnen Rust-Unit-Test auf `[x]` gesetzt werden.
- Der portable Handoff `P0-MAIL-002A-WORKER-SETTINGS` ist geschlossen:
  Request/Offline/lokales Mailverzeichnis/Environment-Overrides/State-Pfad/
  App-Data-Root werden vor dem Worker erfasst. Das ist kein Beleg für die
  Linux-, Visual- oder Großpostfach-Abnahme; weitere `MailStoreService::new()`-
  Workeraufrufe außerhalb Load/Refresh/Load-more bleiben im separaten
  `P0-MAIL-002A-WORKER-STORE-CALLERS`-Folgepaket offen.
- Voraussetzung: ein gebauter Commit mit den portablen Gates aus der
  Ausführungsroadmap und ein inhaltsneutrales Testkonto. Das Testkorpus soll
  mindestens 20 Nachrichten enthalten, darunter drei HTML-Nachrichten über
  1 MiB, eine große Tabelle/Newsletter-CSS, eine fehlerhafte HTML-Mail, eine
  CID-Mail, eine Mail mit externem Bild und eine Mail mit regulären Anhängen.
  Keine persönlichen Mailinhalte oder Zugangsdaten in Logs/Screenshots
  übernehmen.
- Vor jedem Lauf protokollieren:

  ```bash
  git rev-parse HEAD
  uname -a
  pkg-config --modversion gtk4 libadwaita-1 webkitgtk-6.0 camel-1.2 libedataserver-1.2
  cargo fmt --all -- --check
  cargo check --workspace --locked
  cargo test --workspace --locked --no-fail-fast --quiet
  ```

- Startbefehl mit Diagnose:

  ```bash
  RUST_LOG=evolution_gtk4=debug ./target/debug/evolution-gtk4 2>&1 | tee evolution-mail-core.log
  ```

- Ablauf und erwartetes Ergebnis:
  1. Klassische Ansicht mit Konto/Ordnern links, Nachrichtenliste oben und
     Vorschau unten öffnen. Bei 100-%-Skalierung die sichtbare Zahl der
     einzeiligen Zeilen und die Fensterhöhe notieren; Ziel ist die gleiche
     kompakte Größenordnung wie Evolution, ungefähr 40 Nachrichten bei einer
     typischen Desktop-Höhe, ohne unlesbar kleine Schrift.
  2. Innerhalb von 30 Sekunden mindestens 20-mal zwischen den großen
     HTML-Nachrichten wechseln, zusätzlich mit Pfeil-/J-/K-Tastaturwegen und
     einem Ordnerwechsel. Währenddessen Fenster verschieben, suchen und
     Scrollen. Der GTK-Hauptthread muss Eingaben annehmen; nach Abschluss darf
     nur der zuletzt ausgewählte Betreff/Body sichtbar sein. Kein stale Body,
     leerer weißer Reader, dauerhaftes Loading, doppelter Inhalt, Crash oder
     „Programm reagiert nicht“ ist zulässig.
  3. Dieselbe Umschaltserie mit einer fehlerhaften/leer sanitisierten HTML-Mail
     wiederholen. Erwartet wird der explizite Klartext-/Fehlerzustand, während
     ein späteres Ergebnis einer vorherigen Nachricht verworfen bleibt.
  4. Mit `GTK_ICON_THEME=Adwaita` erneut starten und alle sichtbaren
     Mailzeilen, Spaltenköpfe, Sidebar-Wurzeln, Ordner und Detailaktionen
     prüfen. Anhangs-, Nachverfolgungs-, Markierungs- und Wichtigkeitssymbole
     stehen links vor Absender/Betreff und bleiben exakt unter ihren
     Spaltenköpfen. Die erste Zeile ist grau, danach wechseln die Zeilenfarben.
     Klicks auf Absender, Betreff und Datum sortieren und zeigen den Pfeil im
     jeweiligen Kopf; der Breiten-Griff verschiebt Kopf und Daten gemeinsam.
     Fehlende Symbolnamen müssen einen sichtbaren Fallback haben und dürfen
     keine leeren Flächen oder Text-Fehlglyphen erzeugen.
  5. Sidebar prüfen: Konto-Wurzel ohne dekoratives Konto-Icon, flache
     aber erkennbare Ordnerindentation, keine zweite generische lokale
     Ordnergruppe bei einem reinen EDS-Konto, kein Expander bei einer einzelnen
     Nachricht und nur eine ausgerichtete Zählzelle bei echten Konversationen.
     Es erscheinen keine sichtbaren Backendbegriffe. Ein Unread-Badge erscheint nur bei einem
     positiven Ungelesenwert; bei `0` verschwindet es und zeigt nicht den
     Gesamtbestand als Ersatz an.
  6. Eine Nachricht als gelesen und wieder ungelesen markieren, jeweils den
     Backend-/Worker-Erfolg abwarten und danach Refresh, Ordnerwechsel und
     Neustart ausführen. Erfolg muss den Counter entfernen bzw. wiederherstellen.
     Einen absichtlich abgebrochenen oder fehlgeschlagenen Vorgang ausführen;
     dabei darf der Counter nicht vorzeitig verschwinden und kein falscher
     Erfolgsstatus stehen bleiben.
  7. Eine gewöhnliche unverschlüsselte Mail öffnen: die Routine-
     Sicherheitszeile bleibt standardmäßig verborgen und es wird keine
     unnötige Vollprüfung gestartet. Danach die optionale Einstellung aktivieren
     und die Zeile erneut prüfen. Eine echte OpenPGP-/S/MIME-Testmail muss ihre
     Prüfung, Warnung oder ihren Fehler sichtbar machen; der Status darf durch
     die optionale Routineeinstellung nicht unterdrückt werden.
  8. In einer HTML- und einer Klartextmail `Ctrl+Shift+F` verwenden, Unicode-
     Text mit Groß-/Kleinschreibung suchen, Vorheriger/Nächster und Wrap-around
     prüfen und die Leiste mit Escape schließen. Während einer laufenden Suche
     schnell die Nachricht wechseln; weder Highlight noch Trefferzähler der
     alten Nachricht dürfen im neuen Reader erscheinen.
  9. Werkzeugleiste und Status testen: Die klassische Aktionsleiste beginnt
     ganz links oberhalb der Ordnerleiste und reicht über den Mailbereich; im
     Reader stehen die kompakten Antwortaktionen oberhalb des Betreffs. Die
     Statusleisten-Einstellung muss sämtliche Routinezustände live unten ein-
     und ausblenden und nach Neustart erhalten bleiben.
  10. Eine Camel-Nachricht dreimal über die Nachverfolgungsspalte anklicken:
      nach dem ersten Klick müssen `follow-up` und der sichtbare Marker gesetzt
      sein, nach dem zweiten `completed-on` und der grüne Haken, nach dem
      dritten müssen Follow-up/Completed-Tags entfernt sein. Nach jedem Klick
      Refresh, Ordnerwechsel und Neustart prüfen; parallel denselben Ablauf bei
      einem Graph-Konto gegen `flagStatus` = `flagged`, `complete`,
      `notFlagged` abnehmen. Abbruch/Serverfehler dürfen UI und Undo-Historie
      nicht vorzeitig ändern.
- Abnahmekriterien:
  - Alle zehn Ablaufschritte sind mit Commit, Distribution, Versionsdaten,
    Logmarker, anonymisierten Screenshots und Korpus-IDs dokumentiert.
  - Die schnelle Umschaltserie bleibt interaktiv und zeigt niemals ein
    Ergebnis einer nicht mehr ausgewählten Nachricht.
  - Die klassische Dichte, linke Spaltenanordnung, Icon-Fallbacks, flache
    Sidebar und Unread-only-Badges stimmen mit dem GTK3-/Evolution-
    Referenzinventar überein; jede bewusste Abweichung ist als Einstellung
    oder Folgepunkt notiert.
  - Routine-Sicherheitsstatus ist standardmäßig unsichtbar, echte Crypto-
    Zustände bleiben sichtbar, und ein negativer/abgebrochener Counterpfad ist
    belegt.
- Evidenzvorlage:

  ```text
  Commit:
  Distribution / Kernel / Desktop:
  GTK / libadwaita / WebKitGTK / EDS:
  Testkorpus-IDs und Größen:
  Umschaltserie / Dauer / letzter sichtbarer Message-Key:
  sichtbare Zeilenzahl / Skalierung / Sidebar-Befund:
  Icon-Theme / fehlende Icons / Fallback-Befund:
  Counter-Erfolg / Counter-Fehlerpfad:
  Routine-Crypto verborgen / echte Hülle geprüft:
  Logs und Screenshots (inhaltsneutral):
  Verbleibende Abweichungen:
  ```

- `[x]` ist erst zulässig, wenn alle Kriterien einschließlich mindestens
  eines erfolgreichen und eines negativen Umschalt-/Counterpfads erfüllt sind.

### LINUX-P0-MAIL-002 — asynchrone Mailmutationen

- Verknüpft: `P0-STAB-001`.
- Portable Basis: Commits `10b0321` (Lifecycle, Cancellation, Konflikt und
  Rollback) und `d6e7dcc` (GTK-Worker für Einzel/Batch/Auto-Read/Undo/Redo,
  bestätigungsbasierte History und generationensichere Graph-Suche).
- Ablauf: Lesen/Ungelesen, Stern, Wichtigkeit, Label, Archiv, Spam,
  Nicht-Spam, Löschen, Verschieben und Kopieren jeweils einzeln und als Batch
  gegen ein echtes Konto ausführen; parallel Fenster bewegen, Ordner wechseln
  und Suche bedienen.
- Negative Fälle: Netzwerk trennen, Authentifizierung scheitern lassen,
  Providerkonflikt erzeugen und Vorgang abbrechen.
- Erwartung: Hauptfenster bleibt bedienbar; Erfolg wird erst nach
  Backendbestätigung sichtbar; Fehler erzeugen weder Phantomzustand noch
  falschen Undo-Eintrag; Undo/Redo bleibt server- und UI-konsistent.
- Evidenz: Zeitstempel/Logs je Operation, Serververgleich und Commit.

### LINUX-P0-COMP-001 — Entwurf, Antwort und Versand

- Verknüpft: `P0-COMP-001`.
- Portable Basis: Commits `10b0321` (RFC822-/MIME-Komposition) und `d6e7dcc`
  (Autosave-Serialisierung, Recovery, pro-Draft-Sendelock und revisionssicheres
  Worker-Cleanup); Transport- und Ordnerwirkung bleiben Linux-only.
- Ablauf: Neu, Antworten, Allen antworten, Weiterleiten, HTML/Text, Alias,
  Signatur, Inline- und reguläre Anhänge, Autosave, Wiederöffnen und Versand
  gegen Drafts-/Sent-Ordner prüfen.
- Negative Fälle: Offline-Versand, TLS/Auth-Fehler, Abbruch und mehrfaches
  Aktivieren des Senden-Buttons.
- Erwartung: kein Doppelversand; Fehlschlag erhält einen vollständigen
  Entwurf; Erfolg räumt den Entwurf auf und zeigt die Nachricht in Gesendet.
- Evidenz: Message-IDs statt Mailinhalt, Ordnerstatus, Log und Commit.

### LINUX-P0-RECOVERY-001 — EDS-/Netzwerk-Wiederanlauf

- Verknüpft: `P0-RECOVERY-001`.
- Ablauf: Offline/Online umschalten, Netzwerk verlieren, EDS-Factories und
  D-Bus neu starten, Desktop suspendieren und App während eines Loads sauber
  beenden/neu starten.
- Erwartung: keine blockierte Oberfläche, keine doppelten Watcher/Zeilen,
  verständlicher Status, kontrollierter Retry und kein Verlust app-eigener
  Zustände oder originaler Evolution-Daten.
- Evidenz: letzte Logmarker vor/nach Unterbrechung, Prozessstatus und Commit.

## Arbeitsregel für die spätere Linux-Abnahme

Die Punkte dieser Datei werden bewusst erst auf einem echten Linux-System
ausgeführt. Auf macOS werden sie nicht durch Stubs, simulierte Konten oder
voreilig gesetzte `[x]`-Markierungen ersetzt. Für jeden erledigten Punkt muss
der verwendete Commit, die Distribution, die EDS-Version und der konkrete
Testbeleg im Abnahmeprotokoll unten ergänzt werden.

Empfohlene Reihenfolge:

1. Entwicklungsumgebung und native Brücken bauen.
2. EDS-/D-Bus-/Secret-Service-Grundlage mit einem Testkonto prüfen.
3. Erst danach Camel-Laden, Ordner, Paging und Mutationen testen.
4. Versand, Drafts/Sent und Fehlerfälle testen.
5. ECal/EBook, Sicherheit und Packaging abnehmen.

Ein Block gilt erst als abgeschlossen, wenn sowohl der Erfolgsweg als auch
mindestens ein Abbruch-/Fehlerweg reproduziert und dokumentiert wurde.

## 1. Reproduzierbare Linux-Basis

- `[~]` Entwicklungsumgebung dokumentieren und auf einer frischen Maschine
  installieren.
  - Fedora: `evolution-data-server-devel`, `gtk4-devel`,
    `libadwaita-devel`, `webkitgtk6.0-devel`, `gobject-introspection-devel`,
    `meson`, `ninja-build`, `desktop-file-utils`, `appstream`, `rust`,
    `cargo` und ein C-Compiler.
  - Debian/Ubuntu und Arch-Kommandos ergänzen und jeweils die tatsächlich
    benötigte EDS-Version dokumentieren.
  - Prüfen, dass `pkg-config` mindestens
    `camel-1.2`, `libedataserver-1.2`, `libecal-2.0`, `libebook-1.2`,
    `libebook-contacts-1.2`, `gtk4`, `libadwaita-1` und `webkitgtk-6.0`
    findet.
- `[x]` Native Cargo-Kompilierung verifiziert:
  - `cargo check --workspace` ✓ (Fedora 44, EDS 3.60.2, Rust 1.97.1)
  - `cargo test --workspace` ✓ 189/189 Tests bestanden
  - alle drei nativen Brücken (Camel, ECal, EBook) erfolgreich kompiliert;
    dabei drei EDS-3.60-Inkompatibilitäten behoben:
    - `camel_folder_get_uids`/`free_uids` in `find_message_uid_by_id` mit
      `#if EDS_BRIDGE_HAS_DUP_UIDS`-Guard nachgerüstet (Camel ≥ 3.58).
    - `e_cal_client_connect_sync` gibt `EClient*` zurück → `E_CAL_CLIENT()`-
      Cast an allen drei Aufrufstellen ergänzt; `e_cal_client_view_set_flags`
      hat jetzt einen dritten `GError**`-Parameter → `NULL` übergeben.
    - `e_book_client_connect_sync` gibt `EClient*` zurück → `E_BOOK_CLIENT()`-
      Cast an allen drei Aufrufstellen ergänzt; `e_vcard_to_string` hat kein
      Format-Argument mehr → zweiten Parameter entfernt.
  - Drei Rust-Testfehler behoben: `quote_as_safe_text` filtert jetzt
    `<script>`-Inhalt (Sicherheit), `preserve_vcard_extensions` preserviert
    den ersten PHOTO-Eintrag korrekt, `label:` nicht mehr als Ordner-Operator.
- `[x]` Vor jedem weiteren Lauf die native Umgebung protokolliert (2026-08-11):
  - `uname -a` → Linux fedora 7.0.14-201.fc44.x86_64
  - GTK4 4.22.4 / libadwaita 1.9.3 / WebKitGTK 2.52.5
  - camel-1.2 3.60.2 / libedataserver-1.2 3.60.2 / libecal-2.0 3.60.2 / libebook-1.2 3.60.2
  - rustc 1.97.1 (8bab26f4f 2026-07-14) / cargo 1.97.1
  - Desktop: GNOME / Wayland / Fedora 44
- `[x]` Meson-Build und Installation verifiziert:
  - `meson setup builddir` ✓
  - `meson compile -C builddir` ✓ (erzeugt src/evolution-gtk4 Release-Binary in 15s)
  - `meson test -C builddir --print-errorlogs` ✓ (validate-desktop & validate-metainfo bestanden)
  - `meson install -C builddir --destdir "$PWD/stage"` ✓
  - anschließend die installierte Anwendung aus dem Stage-Verzeichnis gestartet.
- `[x]` GSettings, Desktop-Datei und AppStream geprüft:
  - Schema mit `glib-compile-schemas` kompilieren und mit `gsettings` lesen/schreiben ✓
  - `desktop-file-validate` und `appstreamcli validate --no-net` ausgeführt ✓
  - Pango-Ampersand-Escaping in Preferences & EDS-Quellen korrigiert (`&amp;`) ✓
- `[ ]` Wayland als Primärpfad und X11-Fallback testen, inklusive HiDPI,
  mehreren Monitoren, deutschem Gebietsschema und einer frischen Benutzer-
  session ohne vorhandene App-Caches.

### Minimaler Linux-Smoke-Test

Nach einem erfolgreichen Build muss dieser kurze Ablauf ohne manuelle
Codeänderung funktionieren:

```text
1. EDS-/GOA-Dienste starten und Evolution einmal mit dem Testkonto öffnen.
2. Anwendung starten; Konto und mindestens ein echter Ordner erscheinen.
3. Inbox öffnen; eine Nachricht lesen, markieren und als ungelesen setzen.
4. Nachricht in einen Testordner verschieben und wieder zurück verschieben.
5. Antworten, Allen antworten, Weiterleiten und Entwurf speichern öffnen.
6. Anwendung beenden und erneut starten; EDS-Zustand und lokale Einstellungen
   bleiben erhalten.
```

Bei einem Fehler zuerst die native C-Ausgabe, danach den Rust-Statusbereich
und zuletzt die EDS-/D-Bus-Logs sichern. Keine persönlichen Mailinhalte,
Passwörter, OAuth-Tokens oder vollständigen RFC822-Daten in das Repository
oder in CI-Artefakte übernehmen.

### Startdiagnose bei „Programm reagiert nicht“

Für den von manchen Linux-Systemen beobachteten Hänger kurz nach dem Start
zuerst den vorhandenen Diagnosepfad verwenden, bevor weitere Änderungen am
UI vorgenommen werden:

```bash
RUST_LOG=evolution_gtk4=debug ./target/debug/evolution-gtk4 2>&1 | tee evolution-start.log
```

Die folgenden Marker grenzen die Fehlerklasse ein:

- `Deferred WebKitGTK context and view created`: Der Linux-HTML-Renderer ist
  initialisiert. Der Renderer wird absichtlich erst beim Öffnen einer
  Nachricht erzeugt; sein Fehlen direkt nach dem Programmstart ist daher kein
  Fehler.
- `Mail backend returned from EDS/Camel`: Der native Worker ist aus dem
  Camel-Aufruf zurückgekehrt; ein Fehlen dieses Markers deutet auf EDS/Camel,
  Provider-Authentifizierung oder einen nativen Deadlock hin.
- `Applying mail load to GTK models` und `Mail load applied to GTK models`:
  Der erste Marker beginnt den GTK-Hauptthread-Abschnitt, der zweite beendet
  Ordner- und Nachrichtenmodell-Aufbau. Fehlt nur der zweite Marker, muss die
  Zahl der geladenen Ordner/Nachrichten und der Modellaufbau profiliert werden.
  Der aktuelle Code fügt Nachrichten jetzt in 32er-Schritten in die
  GTK4-rs-Modelle ein und baut Threadgruppen sowie Threadzeilen ebenfalls in
  begrenzten Hauptschleifen-Schritten auf. Währenddessen muss der Statusbereich
  `Nachrichten werden aufgebaut …` bzw. eine Fortschrittszahl anzeigen und die
  Oberfläche muss auf Eingabe und Fensterbewegung reagieren. Wenn der zweite
  Marker trotzdem fehlt, ist der letzte Statusbereich-Text zusammen mit einem
  Backtrace zu sichern; dann liegt die Blockierung außerhalb dieses
  inkrementellen Modellpfads (z. B. in einem nativen Provider-Aufruf oder in
  einem einzelnen, unerwartet großen Ordnerbaum).
- `Loaded mail sources`: Die EDS-Quellenerkennung ist abgeschlossen. Ein
  angezeigtes Konto belegt allein noch nicht, dass Camel bereits lesbare
  Ordner oder Nachrichten geliefert hat.
- `Main window presented`, `MailPage initial mail load deferred until after
  first frame` und `Main window layout installed` müssen vor dem ersten
  Backend-Laden erscheinen. Die EDS-Quellenerkennung läuft inzwischen in
  einem eigenen Worker-Kontext; ihr `elapsed_ms`-Wert darf deshalb nicht als
  Beleg für eine blockierte GTK-Hauptschleife interpretiert werden. Hängt das
  Fenster trotzdem vor `Mail backend returned from EDS/Camel`, liegt der Fehler
  im nativen EDS/Camel-Worker oder in dessen Provider-/Credentialpfad.
- Die portable Reminder-Prüfung liest Kalenderdateien und Reminder-Zustand
  inzwischen in einem Worker; ein Linux-Start-Hänger darf daher nicht mehr
  durch diese Vorprüfung erklärt werden. Bleibt der Prozess trotzdem
  unresponsive, den letzten erreichten Marker sichern und den nativen
  EDS/Camel-Worker separat mit einem Backtrace untersuchen.
- Eine automatisch ausgewählte ungelesene Nachricht darf den GTK-Hauptthread
  nicht blockieren: Der Auswahlcallback muss vor `Mail backend returned from
  EDS/Camel` bzw. dem Modellabschluss weiter Eingaben annehmen können. Die
  Markierung als gelesen wird im aktuellen Stand in einem Worker ausgeführt;
  auf Linux zusätzlich mit einem echten Konto prüfen, dass sie nach
  erfolgreicher Camel-/EDS-Bestätigung persistent erscheint und bei
  Authentifizierungs-, Netzwerk- oder Konfliktfehlern sichtbar ungelesen bleibt.
- Keine manuell hinzugefügten `realize`-Overrides mit
  `parent_realize()` in direkten `gtk::Widget`-Unterklassen zurückbringen:
  GTK4-rs installiert den C-Trampoline selbst, der Parent-Vfunc ist für diese
  zusammengesetzten Widgets nicht vorhanden und kann den Prozess beim ersten
  `present()` mit `panic_cannot_unwind` abbrechen.
- Die Evolution-Menüaktionen „Papierkorb leeren“ und „Expunge“ mit einem
  echten EDS/Camel-Konto ausführen: alle Nachrichten des gewählten nativen
  Papierkorbs vollständig laden, die native Camel-Expunge-/Permanent-Delete-
  Operation ausführen, Fehler oder Konflikte sichtbar melden und danach
  Ordnerzähler sowie die GTK4-Listen inkrementell aktualisieren. Der portable
  lokale Pfad ist bereits angebunden; er darf keinen externen Evolution-
  Mailbestand umschreiben.
- Den Online-/Offline-Schalter mit einem echten Konto prüfen: Im Offline-Modus
  dürfen keine neuen Camel-/Graph-Verbindungen oder dauerhaften Watcher
  gestartet werden, der lokale Bestand muss sichtbar bleiben; beim Umschalten
  auf Online sollen Quellen, Ordner, Nachrichten und Status ohne Neustart
  erneut geladen werden und ein Fehler darf nicht als erfolgreicher Sync
  angezeigt werden.

Für die Abnahme zusätzlich den genauen Commit, `uname -a`,
`pkg-config --modversion webkitgtk-6.0 camel-1.2 libedataserver-1.2`,
die EDS-/Camel-Logs sowie den letzten erreichten Marker notieren. Keine
Authentifizierungsdaten oder Mailinhalte in das Log übernehmen.

## 2. EDS-/D-Bus-/Anmeldeumgebung

- `[~]` EDS-Source-Registry über D-Bus auf einem laufenden GNOME-Desktop
  testen.
  - `evolution-source-registry` und die benötigten EDS-Factories müssen
    laufen.
  - Mail-, Kalender-, Aufgaben-, Memo- und Adressbuchquellen anhand ihrer
    echten ESource-Erweiterungen unterscheiden.
  - Deaktivierte, entfernte und unvollständig konfigurierte Quellen dürfen
    nicht als funktionierende Konten angezeigt werden.
- `[ ]` GOA-/OAuth2-Konto testen:
  - ein Google-, Microsoft- oder anderes unterstütztes GOA-Konto anlegen,
    Token-Ablauf erzwingen und den erneuten Login beobachten.
  - sicherstellen, dass Passwörter und OAuth-Tokens ausschließlich über EDS,
    GOA bzw. Secret Service laufen und nie in App-Dateien oder Logs landen.
  - Secret-Service- und gesperrter-Schlüsselbund-Fehler verständlich anzeigen.
- `[ ]` Passwortkonto testen:
  - falsches Passwort, Passwortänderung, TLS-Fehler und abgelaufene
    Zugangsdaten müssen als Backendfehler erscheinen und erneut authentifizierbar
    sein.
- `[ ]` Kontolebenszyklus testen: Quelle anlegen, aktivieren, deaktivieren,
  bearbeiten, entfernen und während der Anwendungslaufzeit neu laden.
- `[ ]` Native Übernahme eines zuvor portable importierten Evolution-Profils
  testen:
  - die aus der Profilkopie übernommenen, nicht-geheimen Kontodaten gegen
    die echte ESource-Registry abgleichen;
  - ESource-Mailkonto, Identität, Submission und Ordnerzuordnung über EDS
    anlegen bzw. aktualisieren, ohne die originale Evolution-Quelle
    unkontrolliert zu überschreiben;
  - Passwort/OAuth2 ausschließlich über EDS, GOA oder Secret Service
    erneut autorisieren und niemals aus dem Importbericht übernehmen;
  - nach Neustart von EDS und Anwendung prüfen, dass UID, Konto, Drafts,
    Gesendet-Ordner und lokale app-eigene Migration konsistent bleiben;
  - die aus `filters.xml` importierten portablen Regeln und alle aus der
    `signature-list.xml` übernommenen Signaturen gegen die native
    Camel-/Composer-Konfiguration abgleichen;
    nicht unterstützte Bedingungen/Aktionen ausdrücklich protokollieren,
    statt sie stillschweigend als native Regeln auszugeben;
  - importierte Alias-Adressen und die Ordnerzuordnung für Inbox, Drafts,
    Gesendet, Archiv, Papierkorb und Junk gegen die echte ESource-
    Identitäts-/Mailfolder-Konfiguration prüfen;
  - Abbruch bei fehlender Berechtigung, ungültiger Quelle und bereits
    vorhandenem Konto als nachvollziehbaren Konflikt anzeigen.
- `[ ]` EDS-Neustart und D-Bus-Unterbrechung testen. Die Anwendung muss den
  Fehler anzeigen, laufende Worker abbrechen oder sicher beenden und nach dem
  Wiederanlauf neu verbinden können.

## 3. Globale Suche und Suchordner

- `[ ]` In der portablen GTK4-rs-Ansicht mit einem lokalen Testbestand prüfen,
  dass `Ctrl`+`Shift`+`F` die globale Suche öffnet und Treffer aus Mail,
  Kalender/Aufgaben/Notizen und Kontakten anzeigt.
- `[ ]` Mit einem echten synchronisierten Konto prüfen, dass globale Treffer
  auch aus Camel-Ordnern sowie aus ECal-/EBook-ClientViews kommen. Dabei darf
  die lokale Suche nicht als Serverabfrage ausgegeben werden: Provider-Fehler,
  Offlinezustand, unvollständig geladene Ordner und UID-Neuzuordnung müssen
  als sichtbarer Status erscheinen.
- `[ ]` Gespeicherte Suchordner gegen `CamelVeeStore`/`CamelVeeFolder`
  verifizieren: Regeln müssen beim Start wiederhergestellt, bei Änderungen
  aktualisiert und bei Fehlern ohne Verlust der lokalen Suchdefinition
  zurückgesetzt werden.

## 4. Native Camel-Mailstrecke

- `[~]` Native Camel-Brücke gegen die auf dem Linux-System installierte
  EDS-Version bauen. Versionsabhängige API-Annahmen, insbesondere UID- und
  Ordnerflags, mit den installierten Headern prüfen. Der Status „wichtig“ darf
  dabei nicht automatisch mit dem Stern-/`CAMEL_MESSAGE_FLAGGED`-Status
  gleichgesetzt werden: die native Nachrichtenstruktur, das C-ABI, das Parsen,
  Schreiben und die Verifikation müssen das providerabhängige Important-/High-
  Priority-/User-Flag korrekt abbilden oder eine fehlende Providerfähigkeit
  ausdrücklich melden.
- `[~]` Echtes Konto laden:
  - Quelle über EDS auswählen.
  - Camel-Service konfigurieren und authentifizieren.
  - Inbox, Unterordner, Archiv, Gesendet, Entwürfe, Spam und Papierkorb
    unterscheiden.
  - Ordner- und Nachrichtenzähler gegen Evolution bzw. `camel`-Werkzeuge
    vergleichen.
  - RFC-822-Inhalt, Header, MIME-Text, HTML und Anhänge mit bekannten
    Testnachrichten vergleichen.
- `[~]` Pagination und Aktualisierung testen:
  - einen Ordner mit deutlich mehr als 1.000 Nachrichten laden; erste Seite,
    exakte Seitengrenze, mehrere „Mehr laden“-Schritte und Refresh prüfen.
    Anonymisierte `(source, folder, UID)`-Listen müssen über Seiten hinweg
    eindeutig sein und der Graph-Pfad darf pro Folgeseite nur das angeforderte
    `$skip`-Fenster übertragen.
  - neue Nachricht, geänderte Flags, verschobene Nachricht, gelöschte
    Nachricht, gelöschter Ordner und umbenannter Ordner während des Laufens
    erzeugen.
  - nach einer Ordnerumbenennung in Evolution im umbenannten Ordner weitere
    Flag-/Löschänderungen auslösen; sie müssen trotz der alten nativen
    Watcher-Ordnerkennung im neuen Pfad landen.
  - Gelesen/Ungelesen extern und in der GTK4-App umschalten und die Badge-Werte
    nach jeder Änderung gegen Evolution vergleichen. Der portable Pfad kann
    Deltas für geladene UIDs berechnen; falls ein Provider Ereignisse für nicht
    geladene UIDs ohne ausreichende Information liefert, muss das native
    Change-Event zusätzlich die autoritativen Ordnerwerte `unread_count` und
    `total_count` übertragen oder gezielt eine Folder-Summary nachladen.
  - mehr als 32 Änderungen in kurzer Folge erzeugen. Der Watcher darf bei
    Queue-Druck weder stoppen noch Ereignisse still verlieren; nach dem Burst
    müssen UI, Evolution und Server denselben UID-/Flag-Zustand zeigen.
  - nach App-Start einen neuen Unterordner anlegen und dort eine Nachricht
    erzeugen. Falls der native Watcher neue Ordner noch nicht registriert,
    `eds_camel_bridge.c` um Folder-Created-/Watcher-Anmeldung und ein explizites
    Resync-Ereignis erweitern; anschließend ohne App-Neustart wiederholen.
  - eine ausgewählte Nachricht am Anfang, in der Mitte und am Ende extern
    löschen. Erwartung: nächster Nachbar, am Ende vorheriger Nachbar, bei leerem
    Ordner vollständig geleerter Reader.
  - prüfen, dass UID-basierte Updates keine Duplikate oder veraltete Zeilen
    erzeugen.
  - während Konto-/Ordnerwechsel, Suche, Refresh und „Mehr laden“ sofort einen
    zweiten Zielordner wählen. Ausschließlich die letzte Generation darf
    sichtbar werden; Refresh darf nicht zum vorherigen Ordner zurückspringen.
- `[~]` Remote-Aktionen mit einem Testkonto testen:
  - gelesen/ungelesen, markiert, wichtig, Spam/Nicht-Spam, Archiv, Verschieben
    und Löschen.
  - Stern/Flagged und Wichtigkeit getrennt in Evolution und auf dem Server
    prüfen; eine nicht unterstützte Camel-Flagsemantik darf weder den Stern
    überschreiben noch als erfolgreicher Wichtigkeitsstatus erscheinen.
  - nach jeder Aktion Serverzustand, UI-Zustand und Konfliktmeldung prüfen.
  - absichtliche Paralleländerung in Evolution erzeugen und den Konfliktpfad
    bestätigen.
- `[~]` Undo/Redo für Verschieben, Löschen und Archivieren testen:
  - Wiederherstellung aus Papierkorb bzw. Archiv.
  - Die Rust-/C-Operationen übertragen neben der ursprünglichen Camel-UID auch
    die RFC-822-Message-ID und können bei einer providerabhängigen UID-Neuvergabe
    im Zielordner nach der stabileren Message-ID suchen.
  - fehlende Nachricht, fehlender Zielordner, veränderter UID und fehlende
    Berechtigung müssen als nicht ausführbare bzw. konflikthafte Aktion enden.
  - den persistenten Verlauf schließen, neu starten und mit maximal 100
    Einträgen wieder laden.
- `[ ]` Native EDS/Camel-Kopieren ergänzen und abnehmen:
  - den Copy-Befehl für eine Nachricht im gleichen und in einem anderen
    Camel-Ordner ausführen und dabei die serverseitig neu erzeugte UID bzw.
    die RFC-822-Message-ID für den Verlauf zurückgeben;
  - Batch-Kopieren, Kopieren in Unterordner, Berechtigungsfehler, Offline-
    Abbruch und Konflikte prüfen;
  - Undo muss ausschließlich die neu angelegte Kopie löschen, die Original-
    nachricht darf nie verändert werden; Redo muss eine neue Kopie mit der
    dann gültigen UID anlegen;
  - Neustart mit persistentem Undo-Verlauf sowie echte IMAP/IMAPX-, EWS- und
    lokale Camel-Provider verifizieren.
- `[~]` Die GTK4-rs-Seitenleiste stellt die aus CamelFolderInfo geladenen
  EDS-Ordner bereits als aufklappbares `GtkTreeListModel` mit stabilen
  Konto-/Ordner-Schlüsseln dar. Auf Linux müssen noch die reale
  Provider-Hierarchie, Ordner-Separatoren und der Lebenszyklus verifiziert
  werden: Erweitern, Einklappen, Umbenennen, Löschen, Neuordnen und
  Kontowechsel müssen ohne Vollreload funktionieren.
- `[ ]` CamelFolderThread bzw. eine gleichwertige servernahe Threadstrecke
  gegen echte RFC-822-Referenzen prüfen. Große Threads, fehlende Referenzen,
  Betreffänderungen und mehrere Konten müssen korrekt gruppiert werden.
- `[~]` Native serverseitige Suche und virtuelle Suchordner gegen Camel/IMAPX
  implementieren und auf echte Ordner, Flags, Paging und Live-Updates testen.
  Der portable Microsoft-Graph-Pfad für kompatible positive Volltext-/Feld-
  prädikate ist bereits in `ROADMAP.md` abgedeckt, ersetzt aber nicht diese
  EDS/Camel-Abnahme. Für den nativen Pfad müssen Camel-Suchsyntax,
  Status-/Datumssemantik, negative Prädikate, CamelVeeStore/CamelVeeFolder,
  gespeicherte Suchordner, UID-Neuzuordnung und Abbruch bei Offline-Fehlern
  ohne Bedeutungsänderung umgesetzt und gegen IMAP/IMAPX verifiziert werden.
- `[ ]` CamelFilterDriver gegen die importierten Regeln abgleichen und den
  manuellen „Jetzt ausführen“-Pfad für echte Camel-Ordner implementieren:
  Reihenfolge, UND/ODER-Bedingungen, Text-, Header-, Label-, Datums- („vor“/
  „nach“), Ordner-, Ungelesen-, Anhangs- und Junk-/Spam-Bedingungen sowie alle
  Aktionen einschließlich getrenntem Stern-/Flag- und Wichtigkeitsstatus,
  Dry-Run/Testbericht,
  Berechtigungsfehler, UID-Neuzuordnung und Wiederaufnahme nach Offline-
  Unterbrechung müssen mit einem Testkonto verifiziert werden. Der portable
  lokale Sidecar-Lauf darf dabei nicht als serverseitige Ausführung gezählt
  werden.
- `[ ]` Netzwerk- und Offlineverhalten testen:
  - Verbindung während Laden, Lesen und Mutation trennen.
  - Retry mit Backoff, Cancellation, Wiederaufnahme und doppelter Aktion
    prüfen.
  - kein Datenverlust und keine falsche Erfolgsmeldung bei Timeout oder
    Authentifizierungsfehler.

### Exchange-/Serverkontofunktionen

- `[~]` Den portablen Graph-Pfad mit einem echten aktivierten Exchange-/Microsoft-
  365-Konto abnehmen: Ordnerbaum und Pagination, Mailtexte und Anhänge,
  MIME-Versand/Entwürfe, gelesen/ungelesen, Stern, Important, Kategorien,
  Verschieben/Papierkorb, Kalender-CRUD mit Serien, Kontakte-CRUD, GAL und
  OOF. Prüfen, dass der Linux-EDS-/GOA-Credentialpfad nicht versehentlich von
  der Token-Injektion umgangen wird und dass Fehler, Timeout, Ablauf und
  fehlende Berechtigungen sichtbar bleiben.
- `[~]` Abwesenheitsnotiz/Auto-Responder gegen ein echtes Exchange-/Microsoft-
  365-Konto abnehmen: Der portable Graph-Client kann Status, Aktivierung/
  Deaktivierung, Start-/Endzeitraum sowie interne und externe Antworttexte
  bereits typisiert lesen und schreiben; offen bleibt die Linux-Abnahme mit
  dem echten EDS-/GOA-Credentialpfad. Dabei müssen diese Werte gespeichert
  und nach einem Neustart wieder geladen werden. Graph/OAuth2 ist für
  Exchange Online der bevorzugte Pfad; EWS darf nur für kompatible
  Legacy-Server verwendet werden.
  Token-Ablauf, fehlende Berechtigung, Offlinezustand, ungültige Zeiträume und
  serverseitige Richtlinien müssen als sichtbare Fehler erscheinen; der
  die portable Graph-/EDS-Strecke darf dabei keinen erfolgreichen Speichervorgang
  vortäuschen.
- `[ ]` GAL-/LDAP-/Exchange-Suche mit dem Composer testen: Timeout,
  Pagination, Dubletten, Offlinefehler, Datenschutzgrenzen und die Trennung
  zwischen lokalem vCard-Autocomplete und serverseitigen Treffern prüfen.

### Belege für den Camel-Block

Für jede getestete Operation mindestens folgende Zustände festhalten:

```text
Vorheriger Ordner / UID / Message-ID:
Ausgeführte Aktion:
Erwarteter Serverzustand:
Tatsächlicher Serverzustand in Evolution:
Tatsächlicher Zustand in Evolution-GTK4:
Ergebnis bei paralleler Änderung:
```

## 5. EDS-Versand und Kontofunktionen

- `[~]` Die native Camel-Schnittstelle löst aus einem EDS-Mailkonto die
  zugehörige Mail-Identität, Submission-Quelle und den konfigurierten
  Transport auf, liest RFC822-Nachrichten ein und ruft den originalen
  `CamelTransport::send_to_sync()`-Pfad auf. Offen sind der Linux-Build gegen
  die installierten Header sowie der Laufzeittest mit Passwort-, OAuth2- und
  unterschiedlichen SMTP/Submission-Providern.
- `[~]` Der Composer hängt RFC822-Entwürfe im nativen Linux-Build in den von
  `ESourceMailComposition` konfigurierten EDS-Drafts-Ordner ein; außerhalb
  eines EDS-Builds verwaltet die GTK4-rs-Oberfläche lokale Entwürfe bereits
  mit Autosave, Auflisten, Öffnen, Aktualisieren, Löschen und Bereinigung
  nach erfolgreichem Versand. Zu prüfen bleiben auf Linux das Ändern, Löschen,
  Wiederöffnen, mehrere Entwurfsrevisionen und der vollständige native
  EDS-Draft-Lebenszyklus ohne doppelte Kopien.
- `[~]` Die GTK4-rs-Compose-Strecke hält To/Cc/Bcc, Anhänge,
  Reply-/Forward-Vorbelegung und lokale HTML-Entwürfe vor. Reply-All übernimmt
  Reply-To/To/Cc ohne Bcc, dedupliziert Empfänger und zitiert den Originaltext
  sicher; Weiterleiten übernimmt Kopfzeilen und Inhalt. Der Composer baut jetzt
  einen getesteten RFC822/MIME-Body, liest Anhänge im Worker und reicht ihn an
  den Linux-Camel-Transport weiter. EDS-Draft-/Sent-Ordner, Inline-CID, die
  Verbindung der lokalen Klartext-Standardsignatur mit dem nativen
  Identitäts-/PGP-Pfad müssen noch gegen echte Provider verifiziert werden;
  lokale Signaturen, Zitat-Einstellungen und Autosave sind
  plattformunabhängig vorhanden.
- `[~]` Der native Versand nutzt bei fehlender Camel-Sent-Bestätigung den von
  `ESourceMailSubmission` konfigurierten Gesendet-Ordner als Fallback. Gegen
  echte Serverantworten müssen Message-ID, providerabhängige Sent-Kopien und
  Fehlerfälle noch auf Linux verifiziert werden; ein bereits vom Transport
  akzeptierter Versand wird dabei nicht fälschlich als fehlgeschlagen gemeldet.
- `[~]` Eingeschränkter historischer EDS-Mail-Smoke für Backend `ews`,
  `imapx`, `pop3`:
  - GOA Session D-Bus Automatische Host-Auflösung (`org.gnome.OnlineAccounts.Exchange`) ✓
  - Automatische Passwort-Abfrage über GOA D-Bus (`org.gnome.OnlineAccounts.PasswordBased`) ✓
  - `CamelOfflineStore` Online-Steuerung in `eds_camel_bridge.c` ✓
  - Ein früher Live-Posteingangstest mit 211 E-Mails und 13 Ordnern wurde
    protokolliert; er ist kein aktueller Vollbeleg für Kontolebenszyklus,
    Providerparität, Auth-/Offline-Fehler oder die drei neuen Transport-
    Kommandos.
- `[ ]` TLS-Zertifikatsfehler, Offline-Versand, SMTP-Authentifizierung und
  Serverlimits testen.

### Versand-/Draft-Abnahme

Mit einem Testkonto ohne produktive Empfänger prüfen:

- eine reine HTML-Mail, eine Mail mit Umlaut im Betreff und eine Mail mit
  mehreren To/Cc/Bcc-Empfängern senden;
- einen Draft speichern, erneut öffnen, ändern, erneut speichern und danach
  löschen; es darf keine unbemerkte zweite Draft-Kopie entstehen;
- eine Mail mit mehreren Anhängen senden und die Kopie in Gesendet öffnen;
- Transport erfolgreich, Sent-Kopie fehlgeschlagen, Authentifizierung
  fehlgeschlagen und Netzwerk während des Versands getrennt jeweils getrennt
  prüfen;
- Message-ID, From, Reply-To, Bcc-Verhalten, MIME-Grenzen und UTF-8-Inhalt
  anhand des empfangenen RFC822-Dokuments kontrollieren.

## 6. ECal: Kalender, Aufgaben und Memos

- Portable Basis: Historischer Commit `d6e7dcc` vereinheitlicht
  Kalenderansichten über RRULE/RDATE/EXDATE, Mehrtagstermine, exklusive
  DTEND-Grenzen und validierte Zeitzonen/Quellen; der aktuelle uncommitted
  Verification-Snapshot umfasst den einmal vollständigen 564/564-Testlauf;
  `GW-TASK-FILTERS` ergänzt
  Filter-/Advanced-/Persistenzverträge. Aufgaben bieten kompaktes Quick-Edit
  für Status, Priorität, Fälligkeit und Kategorien. Native ECal-Wirkung bleibt
  offen.

- `[~]` `GW-MEMO-EDITOR` ist als eigener portabler Schnitt mit exakt diesem
  Produktionsscope in `src/services/calendar_store_service.rs` und
  `src/views/memos_page.rs` implementiert, unabhängig reviewt und
  `BLOCKED_LINUX`; der separate test-only Cross-Surface-RFC5545-Test liegt in
  `src/views/tasks_page.rs`. Der VJOURNAL-Draft
  bewahrt Felder, URI-/BINARY-Attachments, URL, unbekannte Properties/Parameter
  und geordnete `VALARM`s; atomarer/cross-process Store, Save/Discard/Cancel,
  stale/confirmed Mutation und Export sind belegt. Der Cross-Surface-RFC5545-
  Regressionstest vergleicht entfaltete semantische `ATTACH`-Zeilen und prüft
  physische Zeilen mit höchstens 75 Oktetten. Die folgenden Linux-Punkte prüfen
  ausschließlich EDS-Capabilities/ACL, Provider-Attachment-Transfer,
  Change-Monitor/Factory-Restart und Desktop-Aktionen wie Open URL/Print.

- `[~]` Native ECal-Brücke auf Linux gegen echte Kalender-, Aufgaben- und
  Memo-Quellen kompilieren und verbinden.
- `[~]` Create/Modify/Remove mit EDS-UIDs testen. Parallel geänderte
  Komponenten, ungültige iCalendar-Daten, schreibgeschützte Kalender und
  gelöschte Quellen müssen sauber als Konflikt oder Backendfehler erscheinen.
- `[~]` ECalClientView-Signale bei externem Erstellen, Ändern und Löschen in
  den GTK4-rs-Modellen prüfen; keine Duplikate und kein veralteter Detailview.
- `[~]` Die portable Aufgaben-/Memo-Strecke ist für den EDS-unabhängigen Pfad
  bereits persistent: lokale VTODO-/VJOURNAL-Create/Modify/Remove-Operationen,
  VTODO-Statuswechsel, Fälligkeit, Kategorien, Multi-VALARM und die sortierte
  Suchliste müssen auf Linux zusätzlich gegen echte ECal-UIDs, mehrere Quellen,
  schreibgeschützte Quellen, UID-Konflikte und parallele Änderungen abgenommen
  werden. Dabei dürfen lokale app-eigene Komponenten nicht doppelt erscheinen,
  wenn EDS gleichzeitig verfügbar ist.
- `[ ]` `GW-TASK-FILTERS` gegen echte ECal-Quellen abnehmen: Today und
  Next-7/30/365, no-due, starts/due, completed/uncompleted, status, category
  und attachment müssen dieselben Records wie Evolution liefern. EDS-Query-
  Grenzen, Serverfehler, Zeitzone/DST, Monitor-Refresh und die persistierte
  Quellen-/Kategorienauswahl sind getrennt zu protokollieren; der portable
  Query-AST, zwei abgeschlossene unabhängige Reviews und der 564/564-Testlauf
  sind dafür nur Vorbereitung.
- `[ ]` Wiederholungen, Zeitzonen, Teilnehmer, iTIP/iMIP, freie Zeit,
  Einladungsantworten, private Termine, Kategorien, Anhänge und Alarme mit
  realen EDS-Kalendern testen. Dabei auch prüfen, dass die portable UI beim
  Bearbeiten EDS-Teilnehmerparameter und nicht unterstützte VALARM-Typen nicht
  verwirft. Die lokale Übernahme einer `text/calendar`-Einladung und der
  portable RFC-kompatible Antwortentwurf sind kein Ersatz für den nativen
  ACCEPTED/TENTATIVE/DECLINED-Transport: diese Antworten müssen über das
  aktive EDS-/Camel-Konto erzeugt, versendet, in Gesendet/Drafts
  nachvollzogen, mit dem tatsächlichen Server-Teilnehmerstatus abgeglichen
  und nach einem Neustart erneut verifiziert werden.
- `[ ]` Die lokale Freie-Zeiten-Abfrage gegen echte ECal-Quellen erweitern und
  mit nativen EDS-Free/Busy-Daten sowie der Verfügbarkeit mehrerer Teilnehmer
  vergleichen. Zeitzonen, DST, Ganztagstermine, wiederkehrende Termine,
  schreibgeschützte Quellen und abweichende Kalenderberechtigungen müssen in
  der Linux-Abnahme geprüft werden; die portable UI darf die native
  Teilnehmerverfügbarkeit nicht als bereits verifiziert anzeigen.
- `[~]` `GW-ALARMS` an den nativen ECal-Snapshot anschließen: geordnete
  mehrere VALARMs für VEVENT/VTODO/VJOURNAL, sichere DISPLAY-Zustellung,
  Snooze/Erledigt und Wiederanlauf gegen echte EDS-UIDs prüfen. Absolute und
  relative Trigger, `RELATED=END`, REPEAT/DURATION, wiederkehrende Komponenten,
  Quelländerungen, Zeitzonen/DST und EDS-Factory-Neustarts sind zu
  verifizieren. AUDIO/EMAIL/PROCEDURE/Unknown bleiben inert; lokale
  Fallback-Tests ersetzen diese Linux-Abnahme nicht. Das separate portable
  `GW-REMINDER-RUNTIME`-Wiring ist bereits implementiert und unabhängig
  reviewt; seine Desktop-Deep-Link- und Fensteraktionen werden hier gegen
  EReminderWatcher, EDS-ClientViews und den systemweiten Notification-Daemon
  abgenommen.
- `[~]` Native Datenpfade speisen Tages-, Wochen-, Monats- und die portable
  Agendaansicht; die Linux-Laufzeitprüfung gegen Zeitzonenwechsel, DST-Grenzen
  und wiederkehrende Termine sowie das echte Arbeitswochenraster stehen noch
  aus.
- `[ ]` Offline-/Online-Wiederaufnahme des Erinnerungsdienstes testen,
  inklusive Neustart der EDS-Kalenderfactory, mehrfach gestarteter App,
  doppelter EDS-Signale, Sleep/Wakeup und sicherem Zurückziehen zugestellter
  Notifications; der portable Restart-/Claim-State ist dafür nur die
  Vorbedingung.

## 7. EBook: Kontakte, GAL und Adressbücher

- Portable Basis: Historischer Commit `d6e7dcc` liefert kompakte
  quellenneutrale Liste/Detailansicht, Mehrwortsuche, Kategorien und eine
  begrenzte Dublettenerkennung; der aktuelle uncommitted Verification-Snapshot
  umfasst den einmal vollständigen 564/564-Testlauf und `GW-CONTACT-LISTS`
  ergänzt portable vCard-3/4-
  Listen mit Member-UI. Echte EBook-/GAL-Identität und Mutationen bleiben
  offen.

- `[~]` Native EBook-Brücke auf Linux gegen lokale, LDAP-, GOA- und – soweit
  verfügbar – Exchange-Adressbücher kompilieren und verbinden.
- `[~]` Kontakt-Create/Modify/Remove und EBookClientView-Signale mit echten
  UID-Konflikten, schreibgeschützten Büchern und parallelen Änderungen testen.
  Zusätzlich die Dubletten-Zusammenführung mit mehreren E-Mail-/Telefonwerten,
  unbekannten vCard-Erweiterungen, paralleler EDS-Änderung und einem Fehler
  zwischen Modify und Remove testen; ein partieller Remote-Fehler muss sichtbar
  bleiben und darf nicht als atomar oder rückgängig machbar erscheinen.
- `[ ]` `GW-CONTACT-LISTS` gegen lokale, LDAP-, GOA- und Exchange-Adressbücher
  abnehmen: vCard-3-Evolution-Listen und vCard-4-`KIND:group`/`MEMBER`, lokale,
  externe und verschachtelte Mitglieder, ungültige Member, Anzeigenamen,
  Member-UID-Auflösung, ACLs und parallele Änderungen prüfen. Atomare portable
  Store-Tests, zwei abgeschlossene unabhängige Reviews und der 564/564-Testlauf
  ersetzen weder EBook-Capabilities noch Provider-/Linux-Visualabnahme.
- `[ ]` Native EBook-Feldabdeckung prüfen: mehrere Namen, E-Mail-Adressen,
  Telefone, Adressen, Organisation, Kategorien, Notizen, Fotos, Geburtstage,
  strukturierte Zusatzfelder und benutzerdefinierte Felder. Der portable
  Parser/Editor deckt bereits `N`, zusätzliche RFC-Felder und Inline-Fotos
  ab; auf Linux müssen deren Verlustfreiheit, EDS-Medientypen, URI-Fotos,
  große/gefaltete PHOTO-Werte und GAL-/LDAP-Erweiterungen gegen echte Quellen
  verifiziert werden.
- `[~]` Die portable Composer-Verzeichnissuche ist mit eigenem asynchronem
  Exchange-GAL-Button, Timeout, Pagination, Dublettenfilter und sichtbaren
  Offline-/Berechtigungsfehlern implementiert. Auf Linux bleibt die reale
  EDS-/GOA-/LDAP-Abnahme offen: Composer-Autocomplete und GAL/LDAP-Suche mit
  einem echten Verzeichnis, Datenschutzgrenzen und dem nativen Credentialpfad
  testen.

## 8. Sicherheit und Linux-Integration

- `[ ]` GnuPG/CamelCipherContext für OpenPGP erkennen, prüfen, signieren,
  verschlüsseln und entschlüsseln; Schlüsselwahl, Trust, fehlende Schlüssel,
  abgelaufene/revozierte Schlüssel und Passphrase-Dialoge testen.
  *(Der portable GnuPG-/PGP-MIME-Versand sowie die asynchrone Prüfung und
  Entschlüsselung im lokalen MIME-Pfad sind vorbereitet; offen bleibt die
  Abnahme mit GPG-Agent/Pinentry, echten EDS/Camel-Nachrichten und nativer
  CamelCipherContext-Strecke.)*
- `[~]` Den portablen OpenSSL/CMS-S/MIME-Pfad gegen echte Linux-Zertifikate
  abnehmen: Signieren/Verschlüsseln im Composer sowie Verifizieren und
  Entschlüsseln eingehender MIME-Nachrichten sind implementiert; offen sind
  native CamelSMIMEContext-/EDS-Einbindung, Distribution-Zertifikatsspeicher,
  vollständige Trust Chain, Ablauf/Widerruf, PKCS#12-/Token-/Passphrase-Flows
  und die Fehlerfälle fehlender Zwischenzertifikate oder Empfängerzertifikate.
- `[ ]` Remote-Content- und Tracking-Schutz gegen echte HTML-Mails prüfen:
  Bilder, Redirects, JavaScript, externe Links sowie die portable Auflösung
  bekannter `cid:`-Rasteranhänge; prüfen, dass unbekannte IDs, SVG/aktive
  Inhalte und übergroße Inline-Anhänge blockiert bleiben.
- `[ ]` Secret-Service-Berechtigungen, App-Sandbox, Download-Verzeichnis,
  temporäre Dateien und Logredaktion unter einer Flatpak-Sitzung testen.

## 9. Packaging, Flatpak und Release-Verifikation

- `[~]` Flatpak-Manifest vervollständigen:
  - EDS-Version, Runtime-Version und Buildoptionen festlegen.
  - EDS ist auf den Commit `a12493abe06ecaba8207939c8049b7b5b7085ce9`
    gepinnt; `scripts/generate-cargo-sources.sh` erzeugt die noch fehlende
    `cargo-sources.json`, und `scripts/verify-flatpak-manifest.sh` prüft beide
    Dateien statisch.
  - benötigte D-Bus-, Secret-Service-, Netzwerk-, Wayland- und Download-
    Berechtigungen minimal halten.
- `[ ]` Reproduzierbaren Flatpak-Build durchführen:
  - `flatpak-builder --force-clean build-flatpak org.gnome.Evolution4.json`
  - Sandbox starten und EDS-Factories, GOA und Secret Service erreichen.
  - Paket auf einer frischen Linux-VM installieren und aktualisieren.
- `[ ]` Installations-, Upgrade-, Deinstallations- und Rollbackpfad prüfen;
  Benutzerdaten und das originale EDS-Profil dürfen nicht beschädigt werden.
- `[ ]` Linux-CI einrichten mit Build, Clippy, Rust-Tests, C-Syntax-/Warnings-
  as-errors, Meson-Test, AppStream/desktop validation und einem minimalen
  EDS-Smoketest ohne echte Geheimnisse.
- `[ ]` Separaten manuellen Release-Test mit einem echten GNOME-/GOA-Konto
  dokumentieren. Geheimnisse, Mailinhalte und persönliche Quellen-IDs dürfen
  nicht in CI-Artefakte oder Logs gelangen.

## 10. Querliegende Vollparitäts-Blocker

Diese Checkliste sammelt die Linux-Arbeiten, die in mehreren Hauptblöcken
benötigt werden, aber für eine vollständige Evolution-Parität nicht entfallen
dürfen:

- `[ ]` EDS-/D-Bus-/GOA-Lebenszyklus vollständig abnehmen: Quellen anlegen,
  ändern, aktivieren, deaktivieren und entfernen; OAuth2-/Passwortablauf,
  Secret-Service-Fehler, EDS-Neustart, D-Bus-Unterbrechung und native
  Übernahme eines vorhandenen Evolution-Profils verifizieren.
- `[ ]` Mailanbieter-Parität abnehmen: IMAP/IMAPX, POP3, SMTP/Submission,
  EWS/Exchange und Microsoft 365/Graph mit Ordnern, Identitäten,
  serverseitiger Suche, Vee-Suchordnern, Filtern, Threads, Paging,
  Offline-Wiederaufnahme, Konflikten und UID-/Message-ID-Neuzuordnung.
- `[ ]` Den vollständigen Mail-Lebenszyklus gegen echte Konten verifizieren:
  RFC822/MIME, Anhänge und Inline-CID, Draft-Autosave, Wiederöffnen,
  Änderungen, Löschen, Versand, Sent-Kopie, Antworten/Weiterleiten,
  Identitäts-/Signaturauswahl und providerabhängige Fehler.
- `[ ]` Kalender-, Aufgaben- und Memo-Parität abnehmen: EDS-UIDs,
  Wiederholungen, Zeitzonen/DST, Teilnehmer, iTIP/iMIP, Free/Busy,
  private Termine, Anhänge, VALARM, Snooze und Offline-/Factory-Neustart.
- `[ ]` Adressbuch- und Empfänger-Parität abnehmen: vollständige vCard-/EDS-
  Felder, Fotos, Kategorien, lokale Autocomplete-, LDAP-/GAL-/Exchange-
  Suche, Pagination, Datenschutz und schreibgeschützte Quellen.
- `[ ]` Sicherheit und Datenschutz unter Linux abnehmen: OpenPGP/GnuPG,
  S/MIME, Trust-/Widerrufsprüfung, fehlende Schlüssel/Zertifikate,
  Passphrase-Dialoge, Remote-Content, Tracking-Schutz und Secret-Service-
  Berechtigungen.
- `[ ]` Originalgetreue Arbeitsweise verifizieren: Evolution-kompatible
  Konten-/Ordner-/Identitäts-/Signatur-/Filter-/Suchordner-Einstellungen,
  klassische Layoutoptionen, Tastaturbedienung, Barrierefreiheit,
  Lokalisierung und Migration dürfen keinen Workflowwechsel erzwingen.
- `[ ]` Desktop- und Release-Parität abnehmen: Wayland/X11, HiDPI/mehrere
  Monitore, GNOME-Sitzung, Flatpak-Sandbox, Download-/Temporärpfade,
  Performance großer Mailboxen, Speicher-/Thread-Leaks, Upgrade/Rollback,
  signierte Artefakte und reproduzierbare Linux-CI.

### 10.1 Ergänzende, bisher nur implizit erfasste Linux-Lücken

Die folgenden Punkte sind bewusst zusätzlich und konkreter formuliert. Sie
betreffen keine portable Parser- oder GTK-Arbeit, sondern die Stelle, an der
die Anwendung unter Linux mit EDS, Camel, den Provider-Backends, GNOME und dem
Desktop zusammenarbeiten muss. Ein vorhandener Button, ein lokaler Sidecar
oder ein erfolgreicher Fallback zählt bei diesen Punkten nicht als Ersatz für
die native Abnahme.

#### Native ABI-, Provider- und Ressourcenabdeckung

- `[ ]` Eine unterstützte EDS-/GTK-/WebKitGTK-Versionsmatrix festlegen und
  den nativen Build gegen mindestens die älteste und die aktuelle unterstützte
  EDS-Version prüfen. Für jede Version müssen C-ABI-Strukturen, Enum-Werte,
  Nullable-Felder, String-Encoding, `GError`-Ownership und die Freigabe von
  Camel-/ECal-/EBook-Objekten automatisiert geprüft werden. Abweichungen
  dürfen nicht still über ein erfolgreiches Fallback verdeckt werden.
- `[ ]` Die vorbereiteten GIR-Spezifikationen entweder auf Linux generieren
  und als reproduzierbaren Binding-Build verifizieren oder verbindlich
  dokumentieren, welche API dauerhaft über die geprüften C-Adapter läuft.
  Nicht verwendete generierte Bindings dürfen nicht als vorhandene
  Vollabdeckung gelten; der gewählte Pfad muss pro EDS-Version baubar sein.
- `[ ]` Native C-Brücken mit C-Warnungen als Fehlern sowie AddressSanitizer,
  UndefinedBehaviorSanitizer und einem Leak-/Thread-Sanitizer-Lauf prüfen.
  Dabei besonders Camel-UID-Arrays, ECal-/EBook-Signal-Callbacks,
  `GError`-Pfade, Abbruch während blockierender Aufrufe und das Freigeben
  laufender Watcher nach einem EDS-Neustart abdecken.
- `[ ]` Eine providerabhängige Capability-Matrix aus ESource/Camel/ECal/EBook
  ableiten: Lesen, Schreiben, Flags, User-Tags, Suche, Threads, Anhänge,
  Kalenderteilnehmer, Free/Busy, Fotos, Offlinebetrieb und Quoten. Die UI
  muss nicht verfügbare Fähigkeiten ausblenden oder als nicht unterstützt
  melden, statt einen erfolgreichen Vorgang vorzutäuschen.

#### Native Mailordner, Zustände und Empfang

- `[ ]` Vollständige Camel-Ordnerverwaltung testen: Ordner anlegen,
  umbenennen, löschen, abonnieren/abbestellen, als `NOSELECT` behandeln,
  Hierarchietrenner pro Provider auswerten, Sonderordner anhand der
  `CamelFolderInfo`-Flags erkennen und nach externen Änderungen ohne
  Vollreload aktualisieren. Inbox, Drafts, Sent, Archive, Trash, Junk und
  Outbox müssen auch bei providerabhängigen Namen korrekt zugeordnet werden.
- `[ ]` Camel-Quoten und Ordnerfähigkeiten anzeigen und bei fehlender
  Berechtigung verständlich behandeln. Das gilt auch für schreibgeschützte
  Ordner, serverseitiges Expunge, „Papierkorb leeren“, „Junk leeren“ und
  die endgültige Löschung nach einer vorherigen Undo-Aktion.
- `[ ]` Serverseitige Evolution-Labels/User-Tags ergänzen und prüfen:
  mehrere Labels pro Nachricht, Anlegen/Umbenennen/Löschen eines Tags,
  Zuordnung und Entfernung in Einzel- und Batch-Aktionen, Persistenz nach
  Neustart, Suche/Filterung und das getrennte Verhalten von Label, Stern,
  Junk und Important. Provider ohne diese Fähigkeit müssen explizit als
  eingeschränkt erscheinen; ein lokales Sidecar darf nicht als Remote-Label
  ausgegeben werden.
- `[ ]` Junk-/Spam-Parität mit echten Providern prüfen: Junk und Not-Junk,
  Rückverschieben in den ursprünglichen bzw. konfigurierten Ordner,
  serverseitige Junk-Flags/Header, Junk-Filter, Spam-Report und die
  Wechselwirkung mit Suchordnern und Filtern. Die Behandlung darf nicht nur
  vom Ordnernamen abhängen und darf `CAMEL_MESSAGE_JUNK` nicht mit einem
  beliebigen lokalen Status verwechseln.
- `[ ]` Empfangsoptionen und Benachrichtigungen gegen einen laufenden GNOME-
  Desktop prüfen: Abrufintervall, Hintergrundabruf, neue-Mail-Signale,
  Benachrichtigungsgruppierung, Ton/Do-Not-Disturb, Konto-/Ordnerfilter und
  Verhalten beim gesperrten Bildschirm. Nach Netzwerkwechsel, Suspend und
  Neustart des EDS dürfen weder doppelte Benachrichtigungen noch verlorene
  neue Nachrichten entstehen.
- `[ ]` Große MIME-Nachrichten und problematische RFC822-Fälle nativ prüfen:
  Streaming/Abbruch bei großen Anhängen, Charset-/Transfer-Encoding-
  Kombinationen, `message/rfc822`, verschachtelte Multipart-Strukturen,
  Inline-CID, Kalenderteile sowie beschädigte oder unvollständige Nachrichten.
  Der native Pfad muss denselben sicheren Inhalt anzeigen und exportieren wie
  der portable Parser, ohne Daten still abzuschneiden.

#### Identitäten, Versand und serverseitige Kontofunktionen

- `[ ]` Mehrere EDS-Identitäten und Aliase abnehmen: From/Reply-To,
  Standardidentität, Empfänger- und Signaturauswahl, Drafts-/Sent-Zuordnung,
  Bcc, Antwort- und Weiterleitungsregeln sowie die Vermeidung falscher
  Absender bei einem Kontowechsel. Der ausgewählte Alias muss im erzeugten
  RFC822 und in der nativen Sent-Kopie identisch bleiben.
- `[ ]` SMTP-/Submission-Fähigkeiten vollständig prüfen: TLS-Version und
  Zertifikatsfehler, SASL-Mechanismen, Passwort-/OAuth2-Erneuerung,
  Serverlimits, Größenfehler, Proxy-/Netzwerkfehler, Offline-Outbox,
  Wiederaufnahme und die Unterscheidung zwischen Transporterfolg und
  fehlgeschlagener Sent-Kopie.
- `[ ]` DSN-, Zustell- und Lesebestätigungen gegen einen Testtransport prüfen.
  Die Einstellungen müssen sich in den MIME-/SMTP-Headern wiederfinden;
  Provider, die diese Option nicht unterstützen, müssen einen sichtbaren
  Hinweis liefern. Es darf keine Bestätigung als erfolgreich gelten, nur weil
  der Transport die ursprüngliche Nachricht angenommen hat.
- `[ ]` Native Draft-/Outbox-Lebenszyklen unter Abbruch abnehmen:
  Autosave während des Schreibens, Wiederöffnen, parallele Änderungen,
  Anhänge in temporären Dateien, Löschen nach Versand, Offline-Warteschlange,
  erneuter Versand und Aufräumen verwaister Drafts. EDS-Drafts, lokale
  Fallback-Drafts und Camel-Sent-Kopien müssen anhand von Message-ID und
  UID ohne Doppelkopien auseinandergehalten werden.

#### ECal-/EBook-Sonderfälle, die nur mit EDS verifizierbar sind

- `[ ]` ECal-Zeitzonen vollständig abnehmen: lokale/floating Zeiten,
  `DATE`-Werte, `VTIMEZONE`, Systemzeitzone, unterschiedliche Kalender-
  Zeitzonen, DST-Grenzen, Ganztagstermine und verschobene Serien. Die
  Anzeige, Speicherung, Suche, Drag-and-drop-Verschiebung und Erinnerung
  müssen denselben Zeitpunkt ergeben wie Evolution.
- `[ ]` Native ECal-Alarme und Desktop-Benachrichtigungen prüfen:
  `VALARM`-Display, Audio, E-Mail und prozedurale Typen, absolute und
  relative Trigger, `RELATED=END`, Snooze/Dismiss, wiederkehrende Termine,
  Offline-Zustellung und nachträgliches Zurückziehen. Nicht unterstützte
  Alarmtypen müssen erhalten bleiben und dürfen nicht als gelöscht gelten;
  AUDIO, E-Mail, PROCEDURE und unbekannte Actions werden nur auf Roundtrip und
  Providerverhalten geprüft und nie vom portablen Scheduler ausgeführt.
- `[ ]` Kalenderberechtigungen und Quellenmetadaten abnehmen:
  read-only, Schreibrechte, Delegation, Quellefarbe, Sichtbarkeit,
  Standardquelle, Aktualisierungsintervall, abonnierte Kalender und
  serverseitige Free/Busy-Berechtigungen. Eine parallele Änderung muss als
  Konflikt mit erneuter Entscheidung erscheinen.
- `[ ]` iTIP/iMIP mit Organizer und mehreren Teilnehmern Ende-zu-Ende
  prüfen: REQUEST, REPLY, CANCEL, COUNTER, DECLINECOUNTER, Delegation,
  Teilnahmeparameter, private Termine, Konferenz-URLs, Anhänge,
  EDS-/Camel-Versand, Sent/Drafts und den tatsächlich vom Server
  gespeicherten Teilnehmerstatus.
- `[ ]` EBook-vCard-Kompatibilität gegen echte lokale, LDAP-, GOA- und
  Exchange-Quellen prüfen: vCard-3/4-Version, Foldings/Charsets, strukturierte
  Namen, mehrere Typ-Parameter, Kategorien, X-EVOLUTION-Felder, URI-/Inline-
  Fotos, große Fotos, Geburtstage, Beziehungen, IMPP und unbekannte
  Erweiterungen. Create/Modify/Remove muss verlustfrei sein, soweit der
  Provider das Feld unterstützt, und Einschränkungen müssen sichtbar werden.
- `[ ]` LDAP-/GAL-Betrieb abnehmen: TLS/Trust, Filter, Pagination, Timeout,
  serverseitige Sortierung, Dubletten, schreibgeschützte Treffer,
  Datenschutzgrenzen, Offlineverhalten und die klare Trennung zwischen
  lokalem vCard-Autocomplete und einem serverseitigen Verzeichnis.
- `[ ]` Native Duplikat- und Zusammenführungsfälle bei Kontakten prüfen:
  gleiche E-Mail/Telefonwerte aus mehreren Quellen, identische Namen mit
  unterschiedlichen UIDs, konkurrierende Änderungen und Rückgängigmachen.
  Die Anwendung darf keine fremde EBook-UID durch eine lokale Dublette
  überschreiben.

#### Sicherheit, Einstellungen, Migration und Desktop

- `[ ]` OpenPGP/GnuPG unter einer realen GNOME-Sitzung abnehmen:
  GPG-Agent/Pinentry, Schlüsselbund- und Smartcard-Fälle, Schlüsselwahl,
  Trust- und Widerrufsstatus, abgelaufene/fehlende Schlüssel, signierte und
  verschlüsselte Anhänge, Passphrase-Abbruch sowie sichere Fehlerlogs.
- `[ ]` S/MIME gegen den auf der Distribution verfügbaren Zertifikatsspeicher
  prüfen: Import/ Auswahl von Zertifikaten, vollständige Trust Chain,
  Ablauf/Widerruf, fehlende Zwischenzertifikate, verschlüsselte Anhänge,
  Signaturprüfung, Passphrase-/Token-Abbruch und Fehler bei nicht passenden
  Empfängerzertifikaten.
- `[ ]` Die vollständige Evolution-Einstellungs-Matrix auf Linux abgleichen:
  jede Einstellung muss gespeichert, nach Neustart geladen, auf den nativen
  EDS-/Camel-/ECal-/EBook-Vorgang angewendet und bei einer Änderung durch
  Evolution oder GSettings korrekt neu eingelesen werden. Besonders zu
  prüfen sind Abrufintervall, Offlineumfang, Quoten, Junk, DSN/Lesebestätigung,
  Identitäten, Standardquellen, Zeitzonen, Erinnerungen, Farben, Ansichten,
  klassische Layoutpositionen, Tastenkürzel, Schriftarten und externe Inhalte.
- `[ ]` Eine vollständige Migration eines realen Evolution-Profils testen:
  nicht-geheime `.source`-Daten, Identitäten/Aliase, Ordnerzuordnung,
  Filterreihenfolge, Suchordner, Signaturen, Templates, Views, GSettings,
  lokale Mail-/Kalender-/Kontaktpfade und Quellaktivierung. Geheimnisse
  müssen neu über GOA/Secret Service autorisiert werden. Import, Neustart,
  Rollback und ein zweiter Import dürfen keine EDS-Quelle, lokale Daten oder
  app-eigene Zustände unkontrolliert überschreiben.
  *(Der portable Import explizit exportierter nicht-geheimer INI-/GSettings- /
  DConf-Werte ist bereits implementiert; offen bleibt der Linux-Test gegen
  die echte DConf-Datenbank, native EDS-Registrierung und Secret Service.)*
- `[ ]` Originalgetreue Arbeitsabläufe gegen Evolution vergleichen:
  gleiche klassische Mailanordnung, Ordner-/Suchordner-Navigation,
  Kontextmenüs, Einzel-/Batch-Aktionen, Drag-and-drop, Reply/Forward,
  Einladungsantworten, Kontakte aus dem Composer sowie Undo/Redo. Für jede
  Abweichung muss eine absichtliche Einstellung existieren oder die Lücke in
  der Hauptroadmap verbleiben.
- `[ ]` Desktop-Verträglichkeit abnehmen: Wayland und X11, HiDPI und mehrere
  Monitore, GNOME-Skalierung, deutsches/englisches Gebietsschema, RTL und
  IME, Screenreader/AT-SPI, Tastatur-only, Fokusreihenfolge, High-Contrast,
  systemweite Datei-/URI-Handler, Portals, Zwischenablage und
  Benachrichtigungsberechtigungen.
- `[ ]` Crash-Recovery und Diagnose ohne Telemetrie testen: Absturz während
  Camel-/ECal-/EBook-Mutation, SIGTERM/Suspend, Wiederaufnahme ungesendeter
  Drafts, atomare Sidecar-/GSettings-Schreibvorgänge, sichere Logredaktion,
  Diagnosepaket ohne Geheimnisse und Wiederherstellung nach beschädigter
  lokaler Zustandsdatei.
- `[~]` Vollständige Linux-Performanceabnahme durchführen: große Ordner und
  Threads, viele ECal-/EBook-Objekte, große Anhänge, parallele Quellen,
  wiederholtes Öffnen/Schließen von Modulen, Speicherwachstum, Main-Thread-
  Blockierung, Worker-Abbruch und Laufzeit nach EDS-/D-Bus-Neustarts.
  *(Teilweise erledigt: UI-Main-Thread-Blockierungen durch Batching/Splice,
  GTK4-Layout-Oszillationen und die gleichzeitige Initialisierung versteckter
  EDS-Module beim Start reduziert; der Camel-Dauer-Watcher startet erst nach
  dem ersten Mail-Ladeergebnis. Speicherwachstum bei sehr großen Konten und
  die echte Linux-Laufzeitabnahme bleiben offen.)*
- `[ ]` Release- und Upgradepfade auf einer frischen Linux-VM abnehmen:
  Flatpak und native Installation, fehlende optionale Provider, Sandbox-
  Portale, EDS-/GOA-/Secret-Service-Kommunikation, AppStream/Desktop-Datei,
  Signaturprüfung, reproduzierbarer Build, Upgrade, Downgrade, Rollback und
  Deinstallation ohne Beschädigung des originalen EDS-Profils.

## 11. Linux-Abnahmeprotokoll

Für jeden Linux-Lauf sind mindestens folgende Angaben zu protokollieren:

```text
Distribution / Version: Fedora Linux 44
Kernel: 7.0.14-201.fc44.x86_64
Desktop / Display server: GNOME 48 / Wayland (wayland-0)
GTK / libadwaita / WebKitGTK: 4.22.4 / 1.9.3 / 2.52.5
Evolution Data Server: 3.60.2
Camel: 3.60.2
Kontotyp und Authentifizierung: EDS/Camel-Smoke-Konto; GOA-/Passwortpfad
  teilweise verifiziert, Ablauf-/Fehlerfälle bleiben offen
Portable Vorbedingung: aktuelle uncommitted Verification-Runde; Cargo-Format-/
  Check-, strikte Schema- und git-diff-Prüfung grün; portable/macOS-
  Workspace-Testevidenz: 564/564 Tests im finalen unsandboxed Lauf vollständig
  erfolgreich; das ist keine Linux-Abnahme. Ein früherer sandboxed Lauf konnte
  18 Graph-Loopback-Mockserver nicht binden; unabhängige Reviews abgeschlossen;
  `MAIL-TRANSPORT-COMMANDS`, `SET-01` und `GW-MEMO-EDITOR` sowie
  `MAIL-PRINT-ACTION`, `COMPOSER-ACTION-SURFACE`, `GW-ALARMS` und
  `GW-REMINDER-RUNTIME` stehen auf `BLOCKED_LINUX`.
Commit für den konkreten Linux-Lauf: vor dem Lauf eintragen; dieser
  Roadmap-/Auditlauf hat keinen Commit und keinen Push erzeugt
Build-Befehl: cargo check --workspace && cargo test --workspace
Testkonto / Testordner: EDS-/Camel-Smoke-Konto; vollständige Kontolebenszyklus-
  und Fehlerabnahme bleibt in den Abschnitten 2–5 offen
Bekannte Abweichungen:
  - Wayland-/X11-Laufzeit, native EDS-Laufzeittests, Kontolebenszyklus und
    vollständige Offline-/Fehlerabnahme sind noch offen
  - EDS-3.60-API-Fixes in allen drei nativen C-Brücken wurden in `bd5913d`
    behoben (camel_folder_get_uids entfernt, e_cal_client_connect_sync-
    Rückgabetyp geändert, e_vcard_to_string-Format-Parameter entfernt)
  - 3 Rust-Unit-Testfehler wurden korrigiert (script-Content-Filter,
    PHOTO-Preserve, label:-Operator)
```

Die Anwendung darf erst als produktiver Evolution-Ersatz bezeichnet werden,
wenn die kritischen Punkte aus den Abschnitten 2, 3 und 4 auf mindestens einer
unterstützten Linux-Distribution bestanden sind und die Offline-/Fehler- und
Wiederanlaufprüfungen ohne Datenverlust abgeschlossen wurden.
