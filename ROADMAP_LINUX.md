# Linux-/EDS-Roadmap

Diese Datei enthält ausschließlich Arbeiten, die eine Linux-Umgebung mit
GTK4, libadwaita und dem originalen Evolution Data Server (EDS) benötigen.
Sie ergänzt die allgemeine [ROADMAP.md](ROADMAP.md); plattformunabhängige
Rust-, GTK4-rs- und Service-Arbeiten bleiben dort.

Ein Punkt ist erst `[x]`, wenn er auf Linux reproduzierbar gebaut und mit
einem echten EDS-/GNOME-Konto ausgeführt wurde. Ein bestandener Rust-Test auf
macOS oder ein vorhandener UI-Button zählt für diese Datei nicht als Beleg.

Status:

- `[x]` = Linux-Build, Laufzeit, Fehlerpfad und relevante Tests verifiziert
- `[~]` = Adapter oder Testvorbereitung vorhanden, aber Linux-Laufzeitprüfung
  oder ein Teil des Abnahmekriteriums fehlt
- `[ ]` = noch offen

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
  Ordnerflags, mit den installierten Headern prüfen.
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
  - mehr als eine Seite Nachrichten laden.
  - neue Nachricht, geänderte Flags, verschobene Nachricht, gelöschte
    Nachricht, gelöschter Ordner und umbenannter Ordner während des Laufens
    erzeugen.
  - prüfen, dass UID-basierte Updates keine Duplikate oder veraltete Zeilen
    erzeugen.
- `[~]` Remote-Aktionen mit einem Testkonto testen:
  - gelesen/ungelesen, markiert, wichtig, Spam, Archiv, Verschieben und
    Löschen.
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
- `[~]` Die GTK4-rs-Seitenleiste stellt die aus CamelFolderInfo geladenen
  EDS-Ordner bereits als aufklappbares `GtkTreeListModel` mit stabilen
  Konto-/Ordner-Schlüsseln dar. Auf Linux müssen noch die reale
  Provider-Hierarchie, Ordner-Separatoren und der Lebenszyklus verifiziert
  werden: Erweitern, Einklappen, Umbenennen, Löschen, Neuordnen und
  Kontowechsel müssen ohne Vollreload funktionieren.
- `[ ]` CamelFolderThread bzw. eine gleichwertige servernahe Threadstrecke
  gegen echte RFC-822-Referenzen prüfen. Große Threads, fehlende Referenzen,
  Betreffänderungen und mehrere Konten müssen korrekt gruppiert werden.
- `[ ]` Serverseitige Suche und virtuelle Suchordner implementieren und auf
  IMAP/IMAPX gegen echte Ordner, Flags, Paging und Live-Updates testen.
- `[ ]` CamelFilterDriver gegen die importierten Regeln abgleichen und den
  manuellen „Jetzt ausführen“-Pfad für echte Camel-Ordner implementieren:
  Reihenfolge, UND/ODER-Bedingungen, alle Aktionen, Dry-Run/Testbericht,
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

- `[ ]` Abwesenheitsnotiz/Auto-Responder gegen ein echtes Exchange-/Microsoft-
  365-Konto abnehmen: aktuellen Status laden, Aktivierung/Deaktivierung,
  Start-/Endzeitraum sowie interne und externe Antworttexte speichern und nach
  einem Neustart wieder laden. Graph/OAuth2 ist für Exchange Online der
  bevorzugte Pfad; EWS darf nur für kompatible Legacy-Server verwendet werden.
  Token-Ablauf, fehlende Berechtigung, Offlinezustand, ungültige Zeiträume und
  serverseitige Richtlinien müssen als sichtbare Fehler erscheinen; der
  aktuelle gtk4-rs-Platzhalter darf dabei keinen erfolgreichen Speichervorgang
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
- `[x]` EDS Mail backend `ews`, `imapx`, `pop3` Integration:
  - GOA Session D-Bus Automatische Host-Auflösung (`org.gnome.OnlineAccounts.Exchange`) ✓
  - Automatische Passwort-Abfrage über GOA D-Bus (`org.gnome.OnlineAccounts.PasswordBased`) ✓
  - `CamelOfflineStore` Online-Steuerung in `eds_camel_bridge.c` ✓
  - Live-Posteingang mit 211 E-Mails und 13 Ordnern von `user@example.com` verifiziert ✓
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

- `[~]` Native ECal-Brücke auf Linux gegen echte Kalender-, Aufgaben- und
  Memo-Quellen kompilieren und verbinden.
- `[~]` Create/Modify/Remove mit EDS-UIDs testen. Parallel geänderte
  Komponenten, ungültige iCalendar-Daten, schreibgeschützte Kalender und
  gelöschte Quellen müssen sauber als Konflikt oder Backendfehler erscheinen.
- `[~]` ECalClientView-Signale bei externem Erstellen, Ändern und Löschen in
  den GTK4-rs-Modellen prüfen; keine Duplikate und kein veralteter Detailview.
- `[~]` Die portable Aufgaben-/Memo-Strecke ist für den EDS-unabhängigen Pfad
  bereits persistent: lokale VTODO-/VJOURNAL-Create/Modify/Remove-Operationen,
  VTODO-Statuswechsel, Fälligkeit, Kategorien, Display-Alarm und die sortierte
  Suchliste müssen auf Linux zusätzlich gegen echte ECal-UIDs, mehrere Quellen,
  schreibgeschützte Quellen, UID-Konflikte und parallele Änderungen abgenommen
  werden. Dabei dürfen lokale app-eigene Komponenten nicht doppelt erscheinen,
  wenn EDS gleichzeitig verfügbar ist.
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
- `[~]` Den portablen Reminder-Service an den nativen ECal-Snapshot anschließen:
  VALARM-Display-Trigger, Zustellung, 10-Minuten-/1-Stunden-Snooze, Erledigt-
  Aktion und Wiederanlauf gegen echte EDS-UIDs prüfen. Zusätzlich müssen
  absolute Trigger, `RELATED=END`, wiederkehrende Termine, Quelländerungen,
  Zeitzonen/DST und EDS-Factory-Neustarts verifiziert werden; lokale
  macOS-/Fallback-Tests ersetzen diese Linux-Abnahme nicht.
- `[~]` Native Datenpfade speisen Tages-, Wochen-, Monats- und die portable
  Agendaansicht; die Linux-Laufzeitprüfung gegen Zeitzonenwechsel, DST-Grenzen
  und wiederkehrende Termine sowie das echte Arbeitswochenraster stehen noch
  aus.
- `[ ]` Offline-/Online-Wiederaufnahme des Erinnerungsdienstes testen,
  inklusive Neustart der EDS-Kalenderfactory, mehrfach gestarteter App,
  doppelter EDS-Signale und sicherem Zurückziehen zugestellter Notifications.

## 7. EBook: Kontakte, GAL und Adressbücher

- `[~]` Native EBook-Brücke auf Linux gegen lokale, LDAP-, GOA- und – soweit
  verfügbar – Exchange-Adressbücher kompilieren und verbinden.
- `[~]` Kontakt-Create/Modify/Remove und EBookClientView-Signale mit echten
  UID-Konflikten, schreibgeschützten Büchern und parallelen Änderungen testen.
- `[ ]` Native EBook-Feldabdeckung prüfen: mehrere Namen, E-Mail-Adressen,
  Telefone, Adressen, Organisation, Kategorien, Notizen, Fotos, Geburtstage,
  strukturierte Zusatzfelder und benutzerdefinierte Felder. Der portable
  Parser/Editor deckt bereits `N`, zusätzliche RFC-Felder und Inline-Fotos
  ab; auf Linux müssen deren Verlustfreiheit, EDS-Medientypen, URI-Fotos,
  große/gefaltete PHOTO-Werte und GAL-/LDAP-Erweiterungen gegen echte Quellen
  verifiziert werden.
- `[ ]` Composer-Autocomplete und GAL/LDAP-Suche mit Timeout, Pagination,
  Offlinefehlern und Datenschutzgrenzen testen.

## 8. Sicherheit und Linux-Integration

- `[ ]` GnuPG/CamelCipherContext für OpenPGP erkennen, prüfen, signieren,
  verschlüsseln und entschlüsseln; Schlüsselwahl, Trust, fehlende Schlüssel,
  abgelaufene/revozierte Schlüssel und Passphrase-Dialoge testen.
- `[ ]` S/MIME mit Zertifikatsspeicher, Trust Chain, Ablauf/Widerruf,
  Signaturprüfung und Verschlüsselung testen.
- `[ ]` Remote-Content- und Tracking-Schutz gegen echte HTML-Mails prüfen:
  Bilder, Redirects, JavaScript, externe Links sowie die portable Auflösung
  bekannter `cid:`-Rasteranhänge; prüfen, dass unbekannte IDs, SVG/aktive
  Inhalte und übergroße Inline-Anhänge blockiert bleiben.
- `[ ]` Secret-Service-Berechtigungen, App-Sandbox, Download-Verzeichnis,
  temporäre Dateien und Logredaktion unter einer Flatpak-Sitzung testen.

## 9. Packaging, Flatpak und Release-Verifikation

- `[~]` Flatpak-Manifest vervollständigen:
  - EDS-Version, Runtime-Version und Buildoptionen festlegen.
  - echte Commit-SHA/Quellen und `cargo-sources.json` erzeugen und prüfen.
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

## 10. Linux-Abnahmeprotokoll

Für jeden Linux-Lauf sind mindestens folgende Angaben zu protokollieren:

```text
Distribution / Version: Fedora Linux 44
Kernel: 7.0.14-201.fc44.x86_64
Desktop / Display server: GNOME 48 / Wayland (wayland-0)
GTK / libadwaita / WebKitGTK: 4.22.4 / 1.9.3 / 2.52.5
Evolution Data Server: 3.60.2
Camel: 3.60.2
Kontotyp und Authentifizierung: noch nicht getestet (Abschnitt 2 offen)
Commit: c07404e (Complete portable parity block) + lokale Bridge-Fixes (noch nicht committed)
Build-Befehl: cargo check --workspace && cargo test --workspace
Testkonto / Testordner: keines – nur native Kompilierung und Unit-Tests
Bekannte Abweichungen:
  - Meson-Build, GSettings, Wayland-Laufzeit und EDS-Laufzeittests noch offen
  - EDS-3.60-API-Fixes in allen drei nativen C-Brücken notwendig gewesen
    (camel_folder_get_uids entfernt, e_cal_client_connect_sync-Rückgabetyp
     geändert, e_vcard_to_string-Format-Parameter entfernt)
  - 3 Rust-Unit-Testfehler korrigiert (script-Content-Filter, PHOTO-Preserve,
    label:-Operator)
```

Die Anwendung darf erst als produktiver Evolution-Ersatz bezeichnet werden,
wenn die kritischen Punkte aus den Abschnitten 2, 3 und 4 auf mindestens einer
unterstützten Linux-Distribution bestanden sind und die Offline-/Fehler- und
Wiederanlaufprüfungen ohne Datenverlust abgeschlossen wurden.
