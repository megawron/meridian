# Evolution GTK4 — Technical Roadmap & Architecture Plan

Vollständige Architektur-Spezifikation, Feature-Mapping und Entwicklungs-Roadmap für einen modernen Mail- & Groupware-Client als GTK4/libadwaita-Frontend für den Evolution Data Server (EDS).

---

## 1. Architektur & Tech-Stack

### 1.1 Sprach-Empfehlung: **Rust + gtk4-rs / libadwaita-rs**

| Kriterium | Rust (gtk4-rs) | Python (PyGObject) | Vala |
|:---|:---|:---|:---|
| **Typensicherheit** | ✅ Vollständig (Compile-time) | ❌ Duck-Typing | ⚠️ Gut, aber weniger Ökosystem |
| **Performance** | ✅ Zero-Cost Abstractions | ❌ GIL, Interpreter-Overhead | ✅ Compiled |
| **Async/Concurrency** | ✅ tokio/async-std + glib MainContext | ⚠️ asyncio, aber GIL-limitiert | ❌ Kein modernes async |
| **EDS-Anbindung** | ⚠️ GIR-Bindings müssen generiert werden | ✅ GI-Bindings out-of-the-box | ✅ Native GObject |
| **Ökosystem-Reife (GTK4)** | ✅ gtk4-rs 0.9+, libadwaita-rs stabil | ✅ Mature | ✅ Offiziell von GNOME |
| **Langzeit-Wartbarkeit** | ✅ Borrow-Checker, kein UB | ⚠️ Runtime-Fehler | ⚠️ Kleines Community-Ökosystem |

> **Empfehlung: Rust** — Die Kombination aus Compile-Time-Safety, Zero-Cost-Abstractions und dem ausgereiften `gtk4-rs`/`libadwaita-rs`-Ökosystem macht Rust zur besten Wahl für ein langlebiges Desktop-Projekt dieser Größe. Der initiale Aufwand für GIR-Binding-Generierung (EDS) wird durch die langfristige Wartbarkeit mehr als kompensiert.

### 1.2 EDS-Binding-Strategie (Hybridansatz)

Da es keine vorgefertigten Rust-Crates für `libcamel`, `libedataserver`, `libecal` und `libebook` gibt, wird ein **zweistufiger Hybridansatz** verfolgt:

```
┌─────────────────────────────────────────────────────┐
│                  Rust Application                    │
│              (gtk4-rs / libadwaita-rs)               │
├─────────────────────────────────────────────────────┤
│  Tier 1: D-Bus via zbus (schneller Einstieg)        │
│  ├── org.gnome.evolution.dataserver.AddressBook     │
│  ├── org.gnome.evolution.dataserver.Calendar        │
│  └── org.gnome.evolution.dataserver.Sources         │
├─────────────────────────────────────────────────────┤
│  Tier 2: GIR-generierte Bindings (Vollzugriff)      │
│  ├── camel-rs     (← Camel-1.2.gir)                │
│  ├── eds-rs       (← EDataServer-1.2.gir)          │
│  ├── ecal-rs      (← ECal-2.0.gir)                 │
│  └── ebook-rs     (← EBook-1.2.gir)                │
└─────────────────────────────────────────────────────┘
```

**Phase 1 (MVP):** `zbus` für D-Bus-Kommunikation mit EDS. Das erlaubt sofortigen Zugriff auf Kontakte, Kalender und Quellenverwaltung, ohne eigene Bindings generieren zu müssen.

**Phase 2 (Full Parity):** GIR-generierte Bindings für `libcamel` (Low-Level-Mailzugriff, MIME-Parsing, Store/Folder-API), da der D-Bus-Layer für performantes Mail-Handling (große Mailboxen, Streaming) nicht ausreicht.

#### GIR-Generierung: Technischer Ablauf

```bash
# 1. Dev-Pakete installieren (Fedora 44)
sudo dnf install evolution-data-server-devel \
  gobject-introspection-devel gtk4-devel libadwaita-devel \
  webkitgtk6.0-devel rust cargo meson

# 2. GIR-Dateien lokalisieren
ls /usr/share/gir-1.0/Camel-1.2.gir
ls /usr/share/gir-1.0/EDataServer-1.2.gir
ls /usr/share/gir-1.0/ECal-2.0.gir
ls /usr/share/gir-1.0/EBook-1.2.gir

# 3. gtk-rs/gir Tool klonen und Bindings generieren
git clone https://github.com/gtk-rs/gir.git
cd gir && cargo build --release

# 4. Pro Bibliothek: Gir.toml konfigurieren + generieren
```

Beispiel `Gir.toml` für `camel-rs`:

```toml
[options]
library = "Camel"
version = "1.2"
min_cfg_version = "1.2"
target_path = "."
work_mode = "normal"
girs_directories = ["/usr/share/gir-1.0", "./gir-files"]
single_version_file = true
generate_safety_asserts = true

[dependencies]
glib = "0.20"
gio = "0.20"

[[object]]
name = "Camel.Session"
status = "generate"

[[object]]
name = "Camel.Store"
status = "generate"

[[object]]
name = "Camel.Folder"
status = "generate"

[[object]]
name = "Camel.MimeMessage"
status = "generate"

[[object]]
name = "Camel.InternetAddress"
status = "generate"
```

### 1.3 Architekturmuster: **Reactive MVVM + Async Event Loop**

```
┌──────────────────────────────────────────────────────────────┐
│                        GTK4 UI Layer                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐    │
│  │ MailView  │  │ CalView  │  │ContactView│  │ComposeView│   │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘    │
│       │              │              │              │          │
│  ┌────▼──────────────▼──────────────▼──────────────▼────┐    │
│  │              ViewModel / State Layer                  │    │
│  │  (GObject properties + notify signals für Bindings)   │    │
│  │  MailListModel, FolderTreeModel, CalendarModel, ...   │    │
│  └────┬─────────────────────────────────────────────────┘    │
│       │  ↕ property-binding, GListModel, GtkExpression       │
└───────┼──────────────────────────────────────────────────────┘
        │ spawn_future_local() / gio::spawn_blocking()
┌───────▼──────────────────────────────────────────────────────┐
│                     Service / Repository Layer                │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐       │
│  │  MailService  │  │ CalService   │  │ContactService│       │
│  │  (libcamel)   │  │ (ECal/D-Bus) │  │(EBook/D-Bus) │       │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘       │
│         │                  │                  │               │
│  ┌──────▼──────────────────▼──────────────────▼──────┐       │
│  │          EDS Session Manager (Singleton)           │       │
│  │     Verwaltet Source Registry, Auth, Lifecycle     │       │
│  └───────────────────────────────────────────────────┘       │
└──────────────────────────────────────────────────────────────┘
```

**Schlüsselprinzipien:**

1. **Kein Blocking auf dem Main Thread:** Alle EDS-Operationen laufen via `gio::spawn_blocking()` (CPU-bound) oder `glib::spawn_future_local()` (I/O-bound mit async). Ergebnisse werden via `glib::idle_add_local_once()` oder Channel (`async_channel`) zurück zum Main Thread gesendet.
2. **GListModel für performantes Rendering:** `GtkListView` mit `GtkSingleSelection` / `GtkNoSelection` und Factory-Pattern (`GtkSignalListItemFactory` oder `GtkBuilderListItemFactory`). Kein `GtkTreeView` (GTK3-Legacy).
3. **Property Binding:** ViewModel-Properties (`glib::Properties`) werden direkt an Widget-Properties gebunden → automatische UI-Updates.
4. **Command Pattern:** User-Aktionen (Löschen, Verschieben, Markieren) werden als Commands modelliert → Undo/Redo-Fähigkeit.

### 1.4 Frontend-Framework: Relm4

Die UI wird als Relm4-Komponentenbaum umgesetzt. Relm4 bleibt dabei eine
Schicht auf gtk4-rs/libadwaita und ersetzt weder GTK noch die EDS-Adapter.
Der originale Evolution Data Server bleibt vollständig erhalten und ist die
autoritative Quelle für synchronisierte Konten, Ordner, Kalender und
Adressbücher. Backend-, Service- und Domänenmodelle dürfen keine Relm4-Typen
benötigen.

- Jede größere UI-Einheit ist eine `Component` mit eigenem Model sowie
  typisierten Input-/Output-Nachrichten.
- Asynchrone EDS-, Camel- und lokale Store-Aufgaben laufen über Relm4-
  Commands oder Worker und melden Ergebnisse als Nachrichten zurück.
- Wiederholte Listenzeilen werden über Relm4-Factories oder, bei sehr großen
  Mailordnern, über GTK-GListModels mit Relm4-Controllern gerendert.
- Dialoge und Unteransichten werden als Child-Components gekapselt; globale
  Aktionen werden über explizite Nachrichten statt versteckter Widget-
  Seiteneffekte angebunden.
- Der aktuell kompatible Zielstand ist Relm4 0.9.x mit gtk4-rs 0.9 und
  libadwaita-rs 0.7. Ein Upgrade auf spätere Relm4-/GTK-Generationen wird als
  eigener Linux-/Flatpak-Kompatibilitätsschritt behandelt.

---

## 2. Feature-Mapping: Evolution-Kernbereiche → GTK4/Libadwaita

### 2.1 Mail-Modul

| Evolution-Feature | GTK4-Implementierung | EDS-Schnittstelle |
|:---|:---|:---|
| **Ordner-Baumstruktur** | `GtkTreeListModel` + `GtkListView` in der Sidebar. Jeder Ordner ist ein `GObject` mit Properties (Name, Unread-Count, Icon). `GtkTreeExpander` für Verschachtelung. | `CamelStore.get_folder_info()` async |
| **Virtuelle Suchordner** | `CamelVeeFolder` als Backend. In der UI als spezielle Sidebar-Einträge mit Such-Icon. Konfiguration über `AdwDialog` mit Rule-Builder. | `CamelVeeStore`, `CamelVeeFolder` |
| **Mail-Filter & Regeln** | `AdwPreferencesWindow` mit Filter-Editor. Rules als serialisierbare Structs. Engine evaluiert beim Eingang via `CamelFilterDriver`. | `CamelFilterDriver`, `CamelFilterSearchFunc` |
| **S/MIME & PGP** | `CamelGpgContext` / `CamelSMIMEContext` für Sign/Verify/Encrypt/Decrypt. Status-Anzeige im Message-Header als `AdwStatusPage`-ähnliches Banner mit Schloss-/Warn-Icon. | `CamelCipherContext`, `CamelGpgContext` |
| **HTML-Editor (Compose)** | **WebKitGTK 6.0 WebView** im editierbaren Modus (`WebKitWebView::set_editable(true)`) mit Custom-Toolbar. Alternative: `GtkTextView` mit eigenem Rich-Text-Renderer. | `CamelMimeMessage` für Assemblierung |
| **Inline-Anhänge** | `CamelMimePart` mit Content-ID → in WebView über `data:` URIs oder lokale Temp-Dateien eingebettet. Attachment-Bar als `GtkFlowBox` unterhalb der Leseansicht. | `CamelMimePart`, `CamelDataWrapper` |
| **Threaded Conversation View** | `GtkListView` mit `GtkTreeListModel` für Thread-Verschachtelung. Summary-Zeile mit Expander. | `CamelFolderThread` |
| **Schnellsuche & erweiterte Suche** | `GtkSearchBar` + `GtkSearchEntry` in der Header-Bar. Erweiterte Suche als `AdwDialog` mit Predicate-Builder. | `CamelSExp`, `CamelFolderSearch` |

#### HTML-Mail-Rendering: Architektur

```
┌─────────────────────────────────────────────┐
│         WebKitGTK 6.0 (GTK4 native)        │
│  ┌───────────────────────────────────────┐  │
│  │   Sandbox: WebKitWebsiteDataManager   │  │
│  │   ├── Ephemeral Mode (kein Cache)     │  │
│  │   ├── CSP: script-src 'none'          │  │
│  │   ├── No Navigation (about:blank)     │  │
│  │   └── Custom URI Scheme: cid://       │  │
│  │         → Inline-Anhang-Resolver      │  │
│  ├───────────────────────────────────────┤  │
│  │   Dark Mode:                          │  │
│  │   ├── prefers-color-scheme: dark      │  │
│  │   │   (via AdwStyleManager sync)      │  │
│  │   └── Fallback: Injiziertes CSS       │  │
│  │       filter: invert(90%) hue-rotate  │  │
│  ├───────────────────────────────────────┤  │
│  │   Tracking-Schutz:                    │  │
│  │   ├── Remote Images: Blocked by       │  │
│  │   │   default, Banner zum Laden       │  │
│  │   ├── Tracking-Pixel: 1×1 → blocked  │  │
│  │   └── External Links: xdg-open()     │  │
│  └───────────────────────────────────────┘  │
└─────────────────────────────────────────────┘
```

### 2.2 Kalender & Aufgaben

| Evolution-Feature | GTK4-Implementierung | EDS-Schnittstelle |
|:---|:---|:---|
| **Tag/Woche/Monat-Ansicht** | Custom `GtkWidget`-Subklassen: `DayView`, `WeekView`, `MonthView`. Rendering via `GtkSnapshot` / Cairo. Umschaltung über `AdwViewSwitcher`. | `ECal` via D-Bus / `ecal-rs` |
| **Event-Erstellung** | `AdwDialog` mit Start/Ende-Picker (`GtkCalendar` + Time-Spinner), Wiederholung, Alarme, Teilnehmer. | `ECalComponent`, `ICalComponent` |
| **ITIP Meeting-Einladungen** | Im Mail-View: Inline-Banner bei `text/calendar` MIME-Parts. Buttons: „Annehmen / Tentativ / Ablehnen". Klick → ITIP-Response via `ECal`. | `ECalComponent`, ITIP-Utility-Funktionen |
| **Aufgaben (Tasks)** | `GtkColumnView` mit Spalten: Checkbox, Titel, Fällig, Priorität. Inline-Edit. | `ECal` (VTODO), `ECalComponent` |
| **Memos/Notizen** | `GtkTextView` mit Rich-Text in `AdwNavigationPage`. | `ECal` (VJOURNAL) |
| **Mehrere Kalender** | Sidebar-Checkboxes pro Kalender (farbkodiert). `GtkColorDialogButton` für Farbwahl. | `ESource` Registry |

### 2.3 Kontakte

| Evolution-Feature | GTK4-Implementierung | EDS-Schnittstelle |
|:---|:---|:---|
| **Kontakt-Liste** | `GtkListView` + `GtkFilterListModel` mit Suchfilter. `GtkSortListModel` für alphabetische Sortierung. | `EBookClient` via D-Bus |
| **Kontakt-Detail** | `AdwPreferencesPage`-ähnliches Layout mit Gruppen (Name, Email, Telefon, Adresse). Avatar via `AdwAvatar`. | `EContact` |
| **Kontakt-Editor** | `AdwDialog` mit Entry-Fields, dynamischen Gruppen (mehrere Emails/Telefone). | `EContact`, `EVCard` |
| **GAL (Global Address List)** | Autocomplete in Compose-View via `GtkEntryCompletion`-Ersatz (eigenes `GtkPopover` + Async-Suche). | EWS GAL via D-Bus, `EBookClient` |

### 2.4 Enterprise / Exchange (EWS → Microsoft Graph)

> **EWS-Deprecation:** Microsoft beendet EWS für Exchange Online. Die Architektur ist so aufgebaut, dass ein `ExchangeBackend`-Trait existiert mit `EwsBackend` (legacy) und `GraphBackend` (Graph API via OAuth2/MSAL & REST/JSON).

---

## 3. UI/UX-Spezifikation (GNOME HIG + macOS-Ästhetik)

### 3.1 Hauptfenster-Layout

```
┌────────────────────────────────────────────────────────────────────┐
│  AdwApplicationWindow                                              │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │  AdwHeaderBar                                                │  │
│  │  ┌─────────┐ ┌──────────────────────────────┐ ┌───────────┐ │  │
│  │  │ ≡ Menu  │ │  🔍 GtkSearchEntry           │ │ ✉️ 📅 👤  │ │  │
│  │  └─────────┘ └──────────────────────────────┘ │ViewSwitcher│ │  │
│  │                                                └───────────┘ │  │
│  ├──────────────────────────────────────────────────────────────┤  │
│  │  AdwOverlaySplitView (outer)                                 │  │
│  │  ┌────────────┬──────────────────────────────────────────┐   │  │
│  │  │  Sidebar   │  AdwNavigationSplitView (inner)          │   │  │
│  │  │  (280px)   │  ┌──────────────┬───────────────────┐    │   │  │
│  │  │            │  │  Mail-Liste   │  Detail/Lese-     │    │   │  │
│  │  │ ┌────────┐ │  │  (380px)      │  ansicht          │    │   │  │
│  │  │ │Account │ │  │              │                   │    │   │  │
│  │  │ │Switcher│ │  │ ┌──────────┐ │ ┌───────────────┐ │    │   │  │
│  │  │ └────────┘ │  │ │ ListItem │ │ │  From: ...    │ │    │   │  │
│  │  │            │  │ │ ├ Avatar  │ │ │  Subject: ... │ │    │   │  │
│  │  │ 📥 Inbox   │  │ │ ├ Subject │ │ │  Date: ...   │ │    │   │  │
│  │  │   📁 Work  │  │ │ ├ Preview │ │ ├───────────────┤ │    │   │  │
│  │  │   📁 Priv  │  │ │ └ Date   │ │ │               │ │    │   │  │
│  │  │ 📤 Sent    │  │ │──────────│ │ │  WebKitWebView │ │    │   │  │
│  │  │ 📝 Drafts  │  │ │ ListItem │ │ │  (HTML Mail)  │ │    │   │  │
│  │  │ 🗑️ Trash   │  │ │ ...      │ │ │               │ │    │   │  │
│  │  │            │  │ └──────────┘ │ │               │ │    │   │  │
│  │  │ ──────────│  │              │ ├───────────────┤ │    │   │  │
│  │  │ 🔍 Search  │  │              │ │ 📎 Anhänge    │ │    │   │  │
│  │  │  Folders   │  │              │ │ (FlowBox)     │ │    │   │  │
│  │  │            │  │              │ └───────────────┘ │    │   │  │
│  └────────────┴──┴──────────────┴───────────────────┘    │   │  │
│  └──────────────────────────────────────────────────────────┘   │  │
└────────────────────────────────────────────────────────────────────┘
```

---

## 4. Projekt-Roadmap

### Statusregeln

Die Roadmap beschreibt den überprüften Produktstand, nicht die Menge an
angelegten UI-Dateien.

- `[x]` = implementiert, getestet und an ein echtes Backend bzw. einen
  persistenten Zustand angebunden.
- `[~]` = Teilimplementierung, UI-Prototyp oder Backend ohne vollständige
  Integration. Diese Punkte zählen nicht als produktionsfertig.
- `[ ]` = nicht implementiert.

Ein Dialog, eine Demo-Zeile oder ein erfolgreicher Toast ist ausdrücklich kein
Nachweis für eine fertige Funktion. Jede abgeschlossene Funktion braucht einen
negativen Fehlerpfad, Persistenz, reale Daten und mindestens einen automatischen
Test auf der Service-Ebene.

### Ist-Stand nach der ersten Bestandsaufnahme

#### Phase 0 — Foundation & Tooling

- [x] Rust-Workspace, Cargo-Lockfile und GPL-3.0-or-later sind vorhanden.
- [~] Relm4-Kompatibilitätsschnitt mit Root-Komponente, typisierten Messages,
  asynchronem Command und dynamischer Listen-Factory ist verifiziert; die
  produktive Frontend-Migration ist noch nicht abgeschlossen.
- [x] Architekturgrenze festgelegt: EDS/Camel/ECal/EBook werden angebunden und
  bleiben autoritativ; Relm4 ersetzt ausschließlich die GTK4-Oberfläche.
- [~] GTK4/libadwaita/WebKitGTK/EDS sind als Abhängigkeiten beschrieben; der
  Build muss noch in einer reproduzierbaren Linux-/Flatpak-Umgebung verifiziert
  werden. Die GIR-Spezifikationen für Camel, EDataServer, ECal und EBook sind
  vorbereitet, aber noch nicht auf Linux generiert und eingebunden.
- [~] Meson ist vorhanden und das Cargo-Kopieren ist korrigiert; CI, installierte
  GSettings-Schemas und ein vollständiger Pakettest fehlen noch.
- [ ] Flatpak-Manifest reproduzierbar bauen (`sha256` und Cargo-Offline-Quellen
  müssen vollständig und geprüft sein).
- [ ] CI mit Formatierung, Clippy, Unit-Tests, Integrationstests und Flatpak-
  Smoke-Test.

#### Phase 0.5 — Relm4-Frontend-Rewrite

- [x] Framework-Entscheidung und Kompatibilitätsschnitt für die verwendete
  GTK4-/libadwaita-Generation.
- [~] Relm4-Root-Komponente mit Navigation und getrennten Mail-, Kalender-,
  Kontakte-, Aufgaben- und Notizen-Child-Components; globale Aktionen,
  Dialog-Output und vollständige Domain-States fehlen noch.
- [~] Mail-Komponente mit Sidebar, Suchzustand, Mail-Fabrik und lokalem
  Async-Command; Auswahl, Detailansicht, sicherer Viewer, Anhang-Speichern und
  lokale Lese-/Markierungsaktionen, lokales Ordnerfiltern und dynamische
  EDS-Kontozeilen sind angebunden. Der Linux-Start lädt zusätzlich über den
  nativen EDS/Camel-Adapter aktivierte Mailkonten und RFC-822-Nachrichten;
  EDS-Ordnerdarstellung und Camel-Flags sind lesend abgebildet; zentrale
  Mutationen und globale Aktionsausgaben fehlen noch.
- [~] Kalender-, Aufgaben-, Memo- und Kontakte-Komponenten mit typisierten
  Loading-/Refresh-Nachrichten; EDS-Streams, Mutationen und vollständige
  Detailmodelle fehlen noch.
- [~] Relm4-Composer mit To/Cc/Bcc/Betreff, HTML-Editor, lokalem
  DraftStoreService, Anhang-Vormerkung und expliziten Signatur-/Verschlüsselungs-
  Zuständen ist vorhanden; Camel-Transport, echte Anhänge, EDS-Absender und
  Child-Outputs fehlen noch. Filter-, Konto-, Termin- und Kontakte-Dialoge
  müssen noch als Child-Components migriert werden.
- [ ] Altes imperatives Frontend entfernen, sobald jede Ansicht einen
  gleichwertigen Relm4-Nachfolger mit Tests besitzt.

#### Phase 1 — Mail lesen (lokal, erster echter Backend-Schnitt)

- [~] Adaptives GTK4/libadwaita-Drei-Spalten-Layout; die produktive
  Drei-Spalten-Mail-Komponente wird im Relm4-Rewrite neu aufgebaut.
- [~] Sicherer WebKitGTK-Viewer: JavaScript ist deaktiviert, Navigation wird
  abgefangen und externe Bilder sind standardmäßig blockiert.
- [~] EDS-Source-Registry-Abfrage über D-Bus; die `Data`-Keyfiles werden nach
  echten `Mail Account`-/Kalender-/Adressbuch-Erweiterungen klassifiziert und
  gefundene Mailquellen werden als
  GTK-Modell in der Konten-Seitenleiste angezeigt und Kalender-/Adressbuch-
  quellen werden über eine gemeinsame Quellenklassifikation in den jeweiligen
  Seiten dynamisch aufgelistet. Nicht-sensitive letzte Quellenstände können
  bei einem EDS-Ausfall explizit als Cache angezeigt werden; Ordner- und
  Kontenmutationen fehlen noch.
- [~] Lokaler Mail-Store: vorhandene EML-, Maildir- und mbox-Daten werden in
  einem Worker gelesen; RFC-822-Header, MIME-Text/HTML, dekodierte Anhänge und
  stabile lokale Ordner-IDs werden unterstützt. Camel-Indizes, vollständige
  MIME-Semantik und serverseitige Flags fehlen noch.
- [~] Suche, Stern, Ordnerfilter und „gelesen“ existieren im imperativen UI;
  die Relm4-Mailkomponente hat Suche, Auswahl, Detailaktionen und lokale
  Lese-/Markierungszustände bereits angebunden. Für den lokalen Import werden
  diese in einem app-eigenen Sidecar persistiert. EDS-Ordner, Camel/EDS-Flags,
  lokale Ordnerzähler und serverseitige Suchordner fehlen noch.
- [~] Anhang „Speichern unter“ ist für lokal gelesene MIME-Anhänge angebunden;
  EDS-/Remote-Anhänge und Inline-CID-Auflösung fehlen.
- [~] Asynchrones Laden lokaler Nachrichten ohne Blockierung des GTK-Main-
  Threads mit Abbruchsignal und 50er-Batches; echtes Paging, Fortschrittsmodell
  und inkrementelle Änderungen aus Camel fehlen noch.
- [~] Native Linux-Camel-Brücke öffnet die aktivierten EDS-Mailquellen über
  `e_source_camel_configure_service()`, verwendet EDS-/GOA-Credentials und
  lädt Ordner sowie begrenzte RFC-822-Nachrichten in die Relm4-Ansicht. Der
  lokale EML/Maildir/mbox-Importer bleibt der explizite Fallback. Unread-/Flagged-
  Camel-Flags werden lesend übernommen; vollständiges Paging, Threads,
  Änderungsereignisse und Mutationen fehlen noch. Der Linux-Laufzeittest mit
  einem realen GNOME-Konto steht aus.
- [~] Mailaktionen besitzen eine zentrale Backend-Schnittstelle; gelesen/
  ungelesen und markiert werden lokal persistent ausgeführt. Löschen,
  verschieben, archivieren, Spam/Junk, wichtig, Undo und Konfliktbehandlung
  werden vom Nur-Lese-Importer explizit abgelehnt, bis Camel/EDS angeschlossen
  ist.
- [ ] Serverseitige Suche und virtuelle Suchordner.

#### Phase 2 — Konten, Empfang und Mailversand

- [~] Konto-Setup und Composer sind vorhanden; der Relm4-Composer kann
  Entwürfe ohne Kontogeheimnisse in einem app-eigenen Store persistieren und
  der Start zeigt gespeicherte EDS-Mailkonten. EDS-Absender, Camel-Transport
  und echte Draft-/Sent-Folder fehlen noch.
- [~] Sichtbare Lese-/Datenschutz-/PGP-Optionen werden über GSettings
  persistiert, sofern das Schema installiert ist.
- [~] EDS-Quellen dynamisch als Konten anzeigen; das Setup speichert zusätzlich
  nicht-geheime Kontoprofile und kennzeichnet sie bis zur EDS/Camel-Aktivierung
  als ausstehend. Bearbeiten, deaktivieren, entfernen und Ordnerverknüpfung
  fehlen noch.
- [ ] IMAP/IMAPX, POP3, lokale Ordner, SMTP und Exchange/Microsoft-365 mit
  OAuth2, TLS, Passwort-/Secret-Service und Offline-Fehlerzuständen.
- [ ] CamelTransport für echten Versand, EDS-Drafts/Sent-Folder, Bcc/Cc,
  Reply/Reply-All/Forward, Signaturen, Quoting und Autosave.
- [ ] Synchronisationsstatus, Retry/Backoff, Netzwerkwechsel und sauberes
  Herunterfahren.

#### Phase 3 — Kalender, Aufgaben und Memos

- [~] Monatsansicht, Navigation, View-Switcher und Dialoge sind vorhanden;
  die Relm4-Kalenderkomponente zeigt EDS-Quellenstatus und lokale VEVENTs,
  echte ECal-Ansichten fehlen noch.
- [~] Relm4-Aufgaben- und Memo-Komponenten laden lokale VTODO-/VJOURNAL-Daten
  aus iCalendar-Dateien; ECal-Streams und Mutationen fehlen noch.
- [~] EDS-Source-Registry und ESource-basierte Kalenderauswahl werden dynamisch
  angezeigt; ECal-Client/VEVENT-Streams und Mutationen fehlen noch.
- [ ] Tag-/Arbeitswoche-/Woche-/Monat-/Agenda-Ansichten mit echten VEvents.
- [ ] Wiederholungen, Zeitzonen, Teilnehmer, iTIP/iMIP, freie Zeiten,
  Einladungsantworten, Alarme, Kategorien, Anhänge und Offline-Sync.
- [ ] VTODO/VJOURNAL CRUD, Sortierung, Prioritäten, Fälligkeiten und Sync.

#### Phase 4 — Kontakte und Adressbücher

- [~] Relm4-Kontaktkomponente lädt lokale vCards und meldet EDS-Quellenstatus;
  Kontaktliste mit Auswahl, Detail, Suche und Editor-UI wird noch migriert.
- [~] Begrenzter vCard-Parser/Serializer mit Roundtrip-Test.
- [~] EBook-/EDS-Quellen werden dynamisch erkannt; Adressbuch-Stream, CRUD,
  Kategorien, Fotos, mehrere
  Adressen/Telefonnummern und Konfliktauflösung.
- [ ] Autocomplete im Composer, GAL/LDAP/Exchange-Adressbuch und lokale
  Adressbuchauswahl.

#### Phase 5 — Sicherheit und Datenschutz

- [~] WebKit-Sicherheitsgrundlagen sind vorhanden.
- [ ] OpenPGP über GnuPG/Camel: Erkennen, Prüfen, Signieren, Verschlüsseln,
  Entschlüsseln, Schlüsselwahl, Vertrauensstatus und Fehlermeldungen.
- [ ] S/MIME mit Zertifikatskette, Trust Store, Ablauf-/Widerrufsprüfung und
  Zertifikatsauswahl.
- [ ] Remote-Content-Entscheidung pro Nachricht/Konto, Tracking-Schutz,
  sichere CID-Anhänge, Sanitizing, Secret-Service und sichere Logs.

#### Phase 6 — Regeln, Suche und Enterprise

- [~] Filter-, Abwesenheits-, Suchordner- und Exchange-Dialoge existieren als
  UI/Service-Skelette; Filterregeln werden nun versioniert persistent
  gespeichert und unterstützte lokale Eingangsaktionen werden ausgewertet.
- [ ] Camel-Filterdriver mit persistenten Regeln, Reihenfolge, Bedingungen,
  vollständigen Aktionen, Testlauf und Fehlerprotokoll.
- [ ] Vollständige Suchsprache, server-/lokale Suche, gespeicherte Suchen und
  Suchordner mit Live-Aktualisierung.
- [ ] Exchange/Microsoft 365: Graph/OAuth2, Mail/Kalender/Kontakte/GAL/OOF,
  EWS-Kompatibilität nur soweit technisch notwendig.

#### Phase 7 — Release- und Produktqualität

- [ ] Migration von Evolution-Profilen, Konten, Filtern, Signaturen,
  Einstellungen und lokalen Ordnern ohne Datenverlust.
- [ ] Wiederherstellung aus Backup, Import/Export (mbox, EML, ICS, vCard),
  Datenlöschung und Diagnosepaket.
- [ ] Accessibility, Lokalisierung, IME, RTL, HiDPI, Tastaturbedienung,
  Screenreader und GNOME-HIG-Review.
- [ ] Performance-Ziele für große Ordner, Speicher-/Thread-Leaks,
  Crash-Recovery und Telemetrie-freie Fehlerdiagnose.
- [ ] Signierte Release-Artefakte, Flatpak-Review, AppStream-Validierung,
  reproduzierbare Builds und dokumentierte Upgrade-/Rollback-Strategie.

---

## 5. Vollständige Einstellungs-Parität

„Jede Einstellung wie im Original“ wird als eigener Produktbereich behandelt.
Die folgende Matrix ist die Arbeitsgrundlage; ein Eintrag ist erst erledigt,
wenn er gespeichert, beim Start wieder geladen, auf die echte Backend-Operation
angewendet und migrierbar ist.

| Bereich | Abzudeckende Einstellungen | Status |
|:---|:---|:---:|
| Allgemein | Sprache, Zeitzone, Datums-/Zeitformat, Startansicht, Fenstergröße/-position, Maximierung, Benachrichtigungen, Bestätigungsdialoge | `[~]` |
| Mail lesen | Vorschaufenster, Threading, Sortierung, Gruppierung, Zitierstil, Schriftarten, HTML/Text, externe Bilder, Tracking, Markieren als gelesen | `[~]` |
| Mail empfangen | Konten, Aktivierung, Abrufintervall, IMAP-Ordner, Offline-Modus, Synchronisationsumfang, Junk/Spam, Quoten, Empfangsbenachrichtigung | `[ ]` |
| Mail senden | SMTP/Transport, Authentifizierung, TLS, Port, Absender, Antwortadresse, Bcc, DSN/Lesebestätigung, Signatur, Autosave, gesendeter Ordner | `[ ]` |
| Composer | HTML/Text, Formatvorlage, Schrift, Rechtschreibung, Wörterbücher, Zitate, Inline-Anhänge, Vorlagen, Snippets, Entwürfe, Undo/Redo | `[ ]` |
| Konten & Identitäten | mehrere Identitäten, Alias-Adressen, Signaturen, Standardkonto, Ordnerzuordnung, Passwort/Secret-Service, OAuth2 | `[~]` |
| Kalender | Standardkalender, Arbeitswoche, Arbeitstage/-zeiten, Zeitzonen, Wochenstart, Erinnerungen, Snooze, Kategorien, Farben, Wetter/Feiertage | `[ ]` |
| Termine | Wiederholung, Teilnehmer, iTIP/iMIP, freie Zeit, Einladungsantworten, private Termine, Alarme, Konferenzanbieter, Anhänge | `[ ]` |
| Aufgaben & Memos | Standardlisten, Priorität, Status, Fälligkeit, Erinnerungen, Sortierung, Kategorien, VTODO/VJOURNAL-Sync | `[ ]` |
| Kontakte | Standardadressbuch, Autocomplete, GAL/LDAP, vCard-Felder, Kategorien, Fotos, Dubletten, Telefon-/Adressformate | `[ ]` |
| Sicherheit | OpenPGP, S/MIME, Schlüssel/Zertifikate, Trust, Ablauf/Widerruf, Verschlüsselungsstandard, sichere Speicherung, Remote-Content | `[~]` |
| Filter & Suche | Filterregeln, Reihenfolge, Bedingungen/Aktionen, gespeicherte Suchen, Suchordner, Volltext/Server-Suche | `[~]` |
| Oberfläche | Theme, Dark Mode, Dichte, Icons, Sidebar, View-Switcher, Shortcuts, Accessibility, Statusleisten | `[~]` |
| Migration | Evolution-Import, Export, Backup, Profilwechsel, Datenpfade, Upgrade-/Rollback-Verhalten | `[ ]` |

### Abnahmekriterium für den produktiven Ersatz

Das Projekt gilt erst dann als produktiver Evolution-Ersatz, wenn alle
Pflichtbereiche der Matrix implementiert sind, ein bestehendes Evolution-Profil
importiert werden kann, Mail/Kalender/Kontakte/Aufgaben synchron arbeiten und
ein kompletter Offline-/Online-/Fehler-/Wiederanlauf-Zyklus ohne Datenverlust
getestet ist. Bis dahin ist die Anwendung eine technische Vorschau und darf
nicht als vollständiger Ersatz beworben werden.

### Nächster Entwicklungsblock

1. Die native EDS/Camel-Ladestrecke auf Linux gegen ein echtes GNOME-/GOA-
   Konto testen und die geladenen Camel-Ordner/Flags als Modelle anbinden.
2. Die Relm4-Mail-Komponente an Camel-Ordner und -Flags anschließen und danach
   die restlichen globalen Mailaktionen über Child-Outputs führen.
3. EDS-Sources in persistente Konto-/Ordnermodelle und echte ECal-/EBook-
   Streams überführen; die Ergebnisse als Relm4-Commands anbinden.
4. Camel-/EDS-Mailoperationen für Lesen, Flags, Verschieben und Löschen
   integrieren; danach SMTP/Drafts/Sent-Folder und die Dialog-Components
   vervollständigen.
