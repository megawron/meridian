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
- `[ ]` Native Cargo-Kompilierung verifizieren:
  - `cargo check --workspace`
  - `cargo test --workspace`
  - dabei den C-Compiler-Aufruf aus `src/build.rs` und alle drei nativen
    Brücken (Camel, ECal, EBook) tatsächlich kompilieren lassen.
- `[ ]` Meson-Build und Installation verifizieren:
  - `meson setup builddir`
  - `meson compile -C builddir`
  - `meson test -C builddir --print-errorlogs`
  - `meson install -C builddir --destdir "$PWD/stage"`
  - anschließend die installierte Anwendung aus dem Stage-Verzeichnis starten.
- `[ ]` GSettings, Desktop-Datei und AppStream prüfen:
  - Schema mit `glib-compile-schemas` kompilieren und mit `gsettings`
    lesen/schreiben.
  - `desktop-file-validate` und `appstreamcli validate --no-net` ausführen.
  - prüfen, dass der installierte Schema-Pfad von der Anwendung gefunden wird.
- `[ ]` Wayland als Primärpfad und X11-Fallback testen, inklusive HiDPI,
  mehreren Monitoren, deutschem Gebietsschema und einer frischen Benutzer-
  session ohne vorhandene App-Caches.

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
- `[ ]` EDS-Neustart und D-Bus-Unterbrechung testen. Die Anwendung muss den
  Fehler anzeigen, laufende Worker abbrechen oder sicher beenden und nach dem
  Wiederanlauf neu verbinden können.

## 3. Native Camel-Mailstrecke

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
- `[ ]` Vollständige Ordnerhierarchie als `GtkTreeListModel` mit stabilen
  EDS-/Camel-Identitäten anbinden. Erweitern, Einklappen, Umbenennen, Löschen,
  Neuordnen und Kontowechsel müssen ohne Vollreload funktionieren.
- `[ ]` CamelFolderThread bzw. eine gleichwertige servernahe Threadstrecke
  gegen echte RFC-822-Referenzen prüfen. Große Threads, fehlende Referenzen,
  Betreffänderungen und mehrere Konten müssen korrekt gruppiert werden.
- `[ ]` Serverseitige Suche und virtuelle Suchordner implementieren und auf
  IMAP/IMAPX gegen echte Ordner, Flags, Paging und Live-Updates testen.
- `[ ]` Netzwerk- und Offlineverhalten testen:
  - Verbindung während Laden, Lesen und Mutation trennen.
  - Retry mit Backoff, Cancellation, Wiederaufnahme und doppelter Aktion
    prüfen.
  - kein Datenverlust und keine falsche Erfolgsmeldung bei Timeout oder
    Authentifizierungsfehler.

## 4. EDS-Versand und Kontofunktionen

- `[ ]` CamelTransport für SMTP/Submission konfigurieren und mit Passwort- und
  OAuth2-Konten testen.
- `[ ]` Entwurf speichern, ändern, löschen und aus EDS-Drafts wieder öffnen.
- `[ ]` Mail mit To/Cc/Bcc, Reply, Reply-All, Forward, Anhängen, Inline-CID,
  Signatur, Quoting und Autosave versenden.
- `[ ]` Gesendet-Ordner, Serverantwort, Message-ID und Fehler beim Versand
  verifizieren; ein Versand darf bei fehlender Bestätigung nicht als Erfolg
  erscheinen.
- `[ ]` TLS-Zertifikatsfehler, Offline-Versand, SMTP-Authentifizierung und
  Serverlimits testen.

## 5. ECal: Kalender, Aufgaben und Memos

- `[~]` Native ECal-Brücke auf Linux gegen echte Kalender-, Aufgaben- und
  Memo-Quellen kompilieren und verbinden.
- `[~]` Create/Modify/Remove mit EDS-UIDs testen. Parallel geänderte
  Komponenten, ungültige iCalendar-Daten, schreibgeschützte Kalender und
  gelöschte Quellen müssen sauber als Konflikt oder Backendfehler erscheinen.
- `[~]` ECalClientView-Signale bei externem Erstellen, Ändern und Löschen in
  den GTK4-rs-Modellen prüfen; keine Duplikate und kein veralteter Detailview.
- `[ ]` Wiederholungen, Zeitzonen, Teilnehmer, iTIP/iMIP, freie Zeit,
  Einladungsantworten, private Termine, Kategorien, Anhänge und Alarme mit
  realen EDS-Kalendern testen.
- `[ ]` Tages-, Arbeitswochen-, Wochen-, Monats- und Agendaansicht gegen
  Zeitzonenwechsel, DST-Grenzen und wiederkehrende Termine prüfen.
- `[ ]` Offline-/Online-Wiederaufnahme und Erinnerungsdienst testen, inklusive
  Neustart der EDS-Kalenderfactory.

## 6. EBook: Kontakte, GAL und Adressbücher

- `[~]` Native EBook-Brücke auf Linux gegen lokale, LDAP-, GOA- und – soweit
  verfügbar – Exchange-Adressbücher kompilieren und verbinden.
- `[~]` Kontakt-Create/Modify/Remove und EBookClientView-Signale mit echten
  UID-Konflikten, schreibgeschützten Büchern und parallelen Änderungen testen.
- `[ ]` Vollständige vCard-Feldabdeckung prüfen: mehrere Namen, E-Mail-
  Adressen, Telefone, Adressen, Organisation, Kategorien, Notizen, Fotos,
  Geburtstage und benutzerdefinierte Felder.
- `[ ]` Composer-Autocomplete und GAL/LDAP-Suche mit Timeout, Pagination,
  Offlinefehlern und Datenschutzgrenzen testen.

## 7. Sicherheit und Linux-Integration

- `[ ]` GnuPG/CamelCipherContext für OpenPGP erkennen, prüfen, signieren,
  verschlüsseln und entschlüsseln; Schlüsselwahl, Trust, fehlende Schlüssel,
  abgelaufene/revozierte Schlüssel und Passphrase-Dialoge testen.
- `[ ]` S/MIME mit Zertifikatsspeicher, Trust Chain, Ablauf/Widerruf,
  Signaturprüfung und Verschlüsselung testen.
- `[ ]` Remote-Content- und Tracking-Schutz gegen echte HTML-Mails prüfen:
  Bilder, Redirects, JavaScript, `cid:`-Anhänge und externe Links.
- `[ ]` Secret-Service-Berechtigungen, App-Sandbox, Download-Verzeichnis,
  temporäre Dateien und Logredaktion unter einer Flatpak-Sitzung testen.

## 8. Packaging, Flatpak und Release-Verifikation

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

## 9. Linux-Abnahmeprotokoll

Für jeden Linux-Lauf sind mindestens folgende Angaben zu protokollieren:

```text
Distribution / Version:
Kernel:
Desktop / Display server:
GTK / libadwaita / WebKitGTK:
Evolution Data Server:
Camel:
Kontotyp und Authentifizierung:
Commit:
Build-Befehl:
Testkonto / Testordner:
Bekannte Abweichungen:
```

Die Anwendung darf erst als produktiver Evolution-Ersatz bezeichnet werden,
wenn die kritischen Punkte aus den Abschnitten 2, 3 und 4 auf mindestens einer
unterstützten Linux-Distribution bestanden sind und die Offline-/Fehler- und
Wiederanlaufprüfungen ohne Datenverlust abgeschlossen wurden.
