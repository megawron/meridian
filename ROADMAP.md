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

**Phase 1 (MVP):** zbus für die Quellenverwaltung und native C-ABI-Brücken
für die synchronisierten EDS-Domänen. Das erlaubt den produktiven Zugriff auf
Camel- und ECal-Daten, ohne Widget-Code oder eine zweite UI-Schicht in die
Backend-Integration einzuführen.

**Phase 2 (Full Parity):** GIR-generierte Bindings für libcamel
(Low-Level-Mailzugriff, MIME-Parsing, Store/Folder-API), da der D-Bus-Layer für
performantes Mail-Handling (große Mailboxen, Streaming) nicht ausreicht.

Der aktuelle Native-Pfad nutzt bewusst kleine, überprüfte C-ABI-Adapter um die
originalen EDS-Bibliotheken. Camel und ECal liefern dabei thread-sichere,
framework-neutrale Rust-Werte; GIR-Bindings bleiben eine spätere Optimierung
für APIs, die über diese Adapter hinaus benötigt werden.

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
│  │  (libcamel)   │  │ (libecal)    │  │(EBook/D-Bus) │       │
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

### 1.4 Frontend-Framework: GTK4-rs und libadwaita

Die produktive Oberfläche bleibt vollständig bei gtk4-rs/libadwaita. Der
ursprüngliche Evolution Data Server bleibt unverändert erhalten und ist die
autoritative Quelle für synchronisierte Konten, Ordner, Kalender und
Adressbücher. Backend-, Service- und Domänenmodelle bleiben von Widget-
Implementierungsdetails getrennt.

- Die `EvolutionWindow`- und `views::*`-Klassen bilden den produktiven
  GTK4-rs/libadwaita-Komponentenbaum.
- Wiederholte Daten werden über `GListModel`, `GtkSingleSelection`,
  `GtkListView`/`GtkListBox` und `GtkSignalListItemFactory` gerendert.
- Asynchrone EDS-, Camel- und lokale Store-Aufgaben laufen über GLib-Futures,
  Worker-Threads und explizite Channels zurück auf den GTK-Main-Thread.
- Adwaita `ViewStack`, `ToolbarView`, `HeaderBar`, `NavigationSplitView` und
  `OverlaySplitView` bilden die adaptive Evolution-Struktur.
- Backend-Services dürfen keine GTK-Typen benötigen; die UI übersetzt
  ausschließlich fertige, thread-sichere Domänenwerte in GObject-Modelle.

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
| **Tag/Woche/Monat-Ansicht** | Custom `GtkWidget`-Subklassen: `DayView`, `WeekView`, `MonthView`. Rendering via `GtkSnapshot` / Cairo. Umschaltung über `AdwViewSwitcher`. | `libecal-2.0` über den EDS-Adapter |
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

Alle Prüfungen, die zwingend eine Linux-Umgebung mit installierten
EDS-/Camel-/ECal-/EBook-Entwicklungsbibliotheken, laufenden EDS-Factories oder
einem echten GNOME-/GOA-Konto benötigen, sind zusätzlich in der
[Linux-Roadmap](ROADMAP_LINUX.md) aufgeführt. Diese Punkte dürfen auf macOS
weiter vorbereitet, aber erst nach dem Linux-Lauf als verifiziert markiert
werden.

### Ist-Stand nach der ersten Bestandsaufnahme

#### Phase 0 — Foundation & Tooling

- [x] Rust-Workspace, Cargo-Lockfile und GPL-3.0-or-later sind vorhanden.
- [x] Die produktive Oberfläche ist auf GTK4-rs/libadwaita festgelegt; die
  kurzzeitig evaluierte alternative UI-Schicht ist entfernt.
- [x] Architekturgrenze festgelegt: EDS/Camel/ECal/EBook werden angebunden und
  bleiben autoritativ; GTK4-rs/libadwaita ersetzt ausschließlich die UI.
- [~] GTK4/libadwaita/WebKitGTK/EDS sind als Abhängigkeiten beschrieben; der
  Build muss noch in einer reproduzierbaren Linux-/Flatpak-Umgebung verifiziert
  werden. Die GIR-Spezifikationen für Camel, EDataServer, ECal und EBook sind
  vorbereitet, aber noch nicht auf Linux generiert und eingebunden. Die
  konkreten Linux-Abnahmeschritte stehen in [ROADMAP_LINUX.md](ROADMAP_LINUX.md).
- [~] Meson ist vorhanden und das Cargo-Kopieren ist korrigiert; CI, installierte
  GSettings-Schemas und ein vollständiger Pakettest fehlen noch.
- [ ] Flatpak-Manifest reproduzierbar bauen (`sha256` und Cargo-Offline-Quellen
  müssen vollständig und geprüft sein).
- [ ] CI mit Formatierung, Clippy, Unit-Tests, Integrationstests und Flatpak-
  Smoke-Test.

#### Phase 0.5 — GTK4-rs-Frontend-Konsolidierung

- [x] GTK4-rs/libadwaita ist die einzige produktive UI-Abhängigkeit und der
  ursprüngliche `EvolutionWindow`-Komponentenbaum ist wieder der Standard-
  Einstiegspunkt.
- [~] Adaptive Navigation, Mail-Drei-Spalten-Ansicht, sicherer Viewer,
  Kalender, Kontakte, Aufgaben, Memos und Composer sind vorhanden; die neuen
  EDS/Camel-Mail- und ECal-Ladestrecken sind initial in die GTK4-rs-Ansichten
  integriert. ECal-Erstellung für Termine, Aufgaben und Memos sowie die
  VTODO-Statusänderung sind angebunden. Der EBook-Stream sowie native
  Kontaktmutationen für Erstellen, Bearbeiten und Löschen sind ebenfalls an
  die GTK4-rs-Kontaktansicht angeschlossen. ECalClientView- und
  EBookClientView-Signale sowie inkrementelle Updates der Kalender-, Aufgaben-,
  Memo- und Kontaktmodelle sind angebunden; vollständige Backend-Parität fehlt
  noch.
- [~] GObject-Modelle, GListModels und Worker-Channels werden für große
  Datenmengen weiter vereinheitlicht. Vollständige Domain-States, Mutationen
  und Fehlerdialoge fehlen noch.
- [~] Die GTK4-rs-Mailansicht ist mit dem nativen EDS/Camel-Lade- und
  Aktionspfad verbunden. Kalender-, Aufgaben- und Memo-Ansichten laden
  synchronisierte ECal-Komponenten über Worker-Channels; EBook-Streams,
  Kontaktmutationen und die zugehörigen Service-Tests sind begonnen. ECal-
  ClientView-Signale und UID-basierte GListModel-Updates sind angebunden;
  vollständige Domain-States und Backend-Parität fehlen noch.

#### Phase 1 — Mail lesen (lokal, erster echter Backend-Schnitt)

- [~] Adaptives GTK4/libadwaita-Drei-Spalten-Layout; die bestehende
  GTK4-rs-Mailseite wird jetzt mit dem nativen Camel-Service verbunden.
- [~] Sicherer WebKitGTK-Viewer: JavaScript ist deaktiviert, Navigation wird
  abgefangen und externe Bilder sind standardmäßig blockiert.
- [~] EDS-Source-Registry-Abfrage über D-Bus; die `Data`-Keyfiles werden nach
  echten `Mail Account`-/Kalender-/Adressbuch-Erweiterungen klassifiziert und
  gefundene Mailquellen werden als
  GTK-Modell in der Konten-Seitenleiste angezeigt und Kalender-/Adressbuch-
  quellen werden über eine gemeinsame Quellenklassifikation in den jeweiligen
  Seiten dynamisch aufgelistet. Nicht-sensitive letzte Quellenstände können
  bei einem EDS-Ausfall explizit als Cache angezeigt werden; Verwaltung und
  Bearbeitung der EDS-Konten fehlen noch.
- [~] Lokaler Mail-Store: vorhandene EML-, Maildir- und mbox-Daten werden in
  einem Worker gelesen; RFC-822-Header, MIME-Text/HTML, dekodierte Anhänge und
  stabile lokale Ordner-IDs werden unterstützt. Camel-Indizes, vollständige
  MIME-Semantik und serverseitige Synchronisation fehlen noch; der lokale
  Import bleibt bewusst schreibgeschützt.
- [~] Suche, Stern, Ordnerfilter und „gelesen“ existieren im GTK4-rs-UI;
  Detailauswahl sowie Lesen, Markieren und Löschen werden für Remote-Nachrichten
  aus dem Servicepfad in diese Oberfläche zurückgeführt. Für den lokalen Import
  werden Zustände in einem app-eigenen Sidecar persistiert; Remote-Flags werden
  über Camel/EDS gelesen und zurückgeschrieben. Mehrfachauswahl mit Gelesen-,
  Archiv-, Spam-, Lösch- und Verschiebeaktionen ist angebunden; die Ziele
  werden aus der dynamischen EDS-Ordnerliste angeboten. Serverseitige
  Suchordner und die übrigen Remote-Aktionen in der Oberfläche fehlen.
- [~] Anhang „Speichern unter“ ist für lokal gelesene MIME-Anhänge angebunden;
  EDS-/Remote-Anhänge und Inline-CID-Auflösung fehlen.
- [~] Asynchrones Laden lokaler Nachrichten ohne Blockierung des GTK-Main-
  Threads mit Abbruchsignal ist vorhanden; die begrenzten Camel-Seiten werden
  ebenfalls über den Worker in die GTK4-rs-Mailseite geladen. Camel-Pagination,
  deduplizierte Aktualisierung, Refresh, account-spezifische Ordnerauswahl,
  GSettings-gesteuertes Hintergrund-Polling, native Camel-
  Ordneränderungssignale und detaillierte Konto-/Ordner-/Nachrichten-
  Fortschrittsmeldungen sind angebunden. Cancellation innerhalb laufender
  Camel-Aufrufe, UID-basierte inkrementelle Nachrichtenaktualisierung und
  begrenztes Retry/Backoff für Mail-Ladevorgänge sind ebenfalls angebunden.
  Eine vollständige inkrementelle Ordner-/Kontosynchronisation und eine
  einheitliche Retry-/Netzwerkstrategie für alle EDS-Domänen fehlen noch.
- [~] Native Linux-Camel-Brücke öffnet die aktivierten EDS-Mailquellen über
  `e_source_camel_configure_service()`, verwendet EDS-/GOA-Credentials und
  lädt Ordnerzusammenfassungen sowie begrenzte RFC-822-Nachrichten in
  thread-sichere Servicewerte. Die GTK4-rs-Mailseite zeigt die initial geladenen
  Nachrichten, Status-/Ordnerzähler und übernimmt Unread-/Flagged-Camel-Flags;
  Lesen, Markieren und Löschen werden über stabile EDS-/Camel-Referenzen
  zurückgeschrieben. Begrenzte Folge-Seiten, Refresh, Kontoauswahl und
  account-spezifische Camel-Ordnerauswahl werden über denselben Servicepfad
  geladen. Der lokale EML/Maildir/mbox-Importer bleibt der explizite Fallback.
  Archivieren, Verschieben und Spam-Markierung werden über denselben
  Servicepfad ausgeführt; Camel-Änderungs-, Lösch- und Umbenennungssignale
  werden über einen langlebigen nativen Watcher in die Mailansicht geleitet.
  Serverzustände werden nach Mutationen verifiziert und als explizite
  Konflikte zurückgegeben; betroffene Nachrichten werden über ihre Camel-UIDs
  inkrementell nachgeladen oder entfernt. RFC-822-Thread-Identitäten,
  transitive Antwortketten und ein anklickbarer Konversationskontext sind für
  lokale und partiell geladene Camel-Nachrichten angebunden; CamelFolderThread-
  basierte Baumdarstellung, vollständiges Nachladen aller Thread-Mitglieder,
  strukturelle Ordnersynchronisation und der Linux-Laufzeittest mit einem
  realen GNOME-Konto stehen noch aus.
- [~] Mailaktionen besitzen eine zentrale Backend-Schnittstelle. Lokale
  Nachrichten behalten den schreibgeschützten Import mit Sidecar für
  Lesestatus/Markierung; Remote-Nachrichten verwenden Camel für Lesen, Flags,
  Archivieren, Verschieben, Spam-Markierung und Löschen. Batch-Aktionen und
  der zentrale Aktionspfad werden aus dem Servicepfad in die GTK4-rs-Mailansicht
  übernommen. Mehrfachauswahl für Gelesen, Archivieren, Spam und Löschen sowie
  atomare Batch-Zustandsänderungen mit Rollback sind vorhanden. Der
  Command-Stack unterstützt erfolgreiche Lese-/Flag-Änderungen inklusive
  Undo/Redo und Konfliktschutz; Verschieben verwendet einen reversiblen
  Command auch in Batch-Aktionen. Der letzte Verlauf mit bis zu 100 Befehlen
  wird atomar im Benutzer-Datenverzeichnis gespeichert und beim Start wieder
  geladen. Inverse Undo-Operationen für Delete/Archive verwenden jetzt native
  Restore-Operationen aus Papierkorb bzw. Archiv und prüfen Quell-/Zielzustand.
  Backend-Locations tragen zusätzlich die RFC-822-Message-ID; der native Pfad
  kann bei verlorener Camel-UID danach suchen. Die Verifikation dieser
  providerabhängigen UID-Neuvergabe auf Linux sowie komplexe Ordneränderungen
  fehlen noch. Konflikte aus dem nativen Camel-Backend werden als eigener
  Fehlerzustand behandelt.
- [ ] Serverseitige Suche und virtuelle Suchordner.

#### Phase 2 — Konten, Empfang und Mailversand

- [~] Konto-Setup und Composer sind vorhanden; der GTK4-rs-Composer kann
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
- [~] Synchronisationsstatus, Cancellation und begrenztes Retry/Backoff sind
  für den nativen Mail-Ladepfad vorhanden; Netzwerkwechsel, domänenweite
  Offline-/Online-Zustände, vollständige Wiederaufnahme und sauberes
  Herunterfahren fehlen noch.

#### Phase 3 — Kalender, Aufgaben und Memos

- [~] Monatsansicht, Navigation, View-Switcher und Dialoge sind vorhanden;
  die GTK4-rs-Kalenderansicht zeigt EDS-Quellenstatus und lädt VEVENTs aus
  aktivierten ECal-Quellen und kann neue VEVENTs über den nativen ECal-Adapter
  speichern. Bearbeiten und Löschen sind für geladene VEVENTs ebenfalls an den
  nativen Adapter angebunden. Die vollständigen Tages-/Wochen-/Agenda-Modelle
  und Wiederholungen fehlen noch.
- [~] GTK4-rs-Aufgaben- und Memo-Ansichten laden VTODO-/VJOURNAL-Komponenten
  über die native libecal-2.0-Brücke aus den aktivierten EDS-Quellen. Neue
  VTODOs/VJOURNALs, VTODO-Statusänderungen und vollständiges VTODO-Bearbeiten
  werden persistent über EDS geschrieben. Der lokale iCalendar-Importer bleibt
  ein expliziter Fallback; inkrementelle ECal-Signale sind für die drei
  Listenmodelle angebunden, vollständige Erinnerungs-/Aufgabenlisten-Parität
  fehlt noch.
- [~] EDS-Source-Registry und ESource-basierte Kalenderauswahl werden dynamisch
  angezeigt; Kalender-, Aufgaben- und Memo-Quellen werden per
  e_cal_client_connect_sync() gelesen, inklusive Quellidentität und
  Backend-Warnungen. Der native Adapter liefert bereits Create/Modify/Remove-
  Operationen mit Konfliktstatus; UI-seitige Bearbeiten-/Löschen-Flows sind für
  Termine, Aufgaben und Notizen angebunden. Eine vollständige
  Konfliktauflösung fehlt noch; die ECal-Signalaktualisierung ist umgesetzt.
- [ ] Tag-/Arbeitswoche-/Woche-/Monat-/Agenda-Ansichten mit echten VEvents.
- [ ] Wiederholungen, Zeitzonen, Teilnehmer, iTIP/iMIP, freie Zeiten,
  Einladungsantworten, Alarme, Kategorien, Anhänge und Offline-Sync.
- [~] VTODO/VJOURNAL-Erstellung, VTODO-Status, Notizbearbeitung/-löschung,
  VTODO-Bearbeitung, Prioritäten, Fälligkeiten und Notizlöschung werden über
  den EDS-Adapter persistiert. ECal-Signalaktualisierung ist angebunden;
  Sortierung, Erinnerungseditoren und vollständige Aufgabenlisten-Parität
  fehlen noch.

#### Phase 4 — Kontakte und Adressbücher

- [~] GTK4-rs-Kontaktansicht lädt Kontakte jetzt über die native EBook-/EDS-
  Brücke aus aktivierten Adressbüchern und fällt außerhalb von Linux explizit
  auf lokale vCards zurück; Auswahl, Detail, Suche sowie native Erstellen-,
  Bearbeiten- und Löschen-Flows sind vorhanden. EBookClientView-Signale werden
  UID-basiert in die Kontaktliste eingepflegt; der Linux-Laufzeittest und die
  vollständige vCard-Feldparität fehlen noch.
- [~] Begrenzter vCard-Parser/Serializer mit Roundtrip-Test.
- [~] EBook-/EDS-Quellen werden dynamisch erkannt; der native Adressbuch-
  Stream liefert vCards mit Quellen- und Kontakt-UIDs; der native Adapter
  unterstützt bereits Create/Modify/Remove mit Konfliktstatus. Kategorien,
  Fotos, mehrere Adressen/Telefonnummern und Konfliktauflösung fehlen noch.
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

1. Die native EDS/Camel-Ladestrecke, UID-basierte Signalaktualisierung und den
   Signal-Watcher auf Linux gegen ein echtes GNOME-/GOA-Konto testen. Die
   vollständige Checkliste dafür befindet sich in
   [ROADMAP_LINUX.md](ROADMAP_LINUX.md); danach strukturelle Ordner-/Konto-
   synchronisation, Netzwerkwechsel und eine domänenweite Offline-/Online-
   Wiederaufnahme ergänzen.
2. Das GTK4-rs-Mailmodell um ein echtes CamelFolderThread-/TreeListModel-
   Modell, providerabhängige UID-Neuzuordnung nach Transfers und sichtbare
   Konflikt-/Restore-Fehleranzeigen erweitern.
3. Die ECal-/EBook-Mutationsstrecke um echte Konfliktauflösung und vollständige
   vCard-Felder erweitern und die Linux-Laufzeit mit einem echten Konto
   verifizieren.
4. Camel-Transport, EDS-Drafts/Sent-Folder und die Dialog-Components für
   Konto-, Empfangs- und Versandoptionen vervollständigen.
