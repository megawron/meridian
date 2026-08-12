# Evolution GTK4 — Technical Roadmap & Architecture Plan

Vollständige Architektur-Spezifikation, Feature-Mapping und Entwicklungs-Roadmap für einen modernen Mail- & Groupware-Client als GTK4/libadwaita-Frontend für den Evolution Data Server (EDS).

> **Dokumentrolle:** Diese Datei ist die vollständige Produktspezifikation und
> der belegte Paritätsstatus. LLM-Agenten beginnen konkrete Arbeit in der
> atomaren [Ausführungsroadmap](ROADMAP_EXECUTION.md). Ausschließlich auf Linux
> prüfbare Implementierung und Abnahme steht in der
> [Linux-/EDS-Roadmap](ROADMAP_LINUX.md). Der Produktumfang darf aus keiner
> dieser Sichten verkleinert werden.

> **Belegter Zwischenstand `d6e7dcc`:** Die tägliche portable GTK4-Strecke für
> sicheren Mail-Viewer, asynchrone Mailmutationen einschließlich Undo/Redo,
> Composer-Recovery/Doppelsendeschutz sowie kompakte Kalender-, Kontakt- und
> Aufgabenansichten ist mit 387/387 Tests und Workspace-Build verifiziert.
> Echte WebKitGTK-, EDS/Camel/ECal/EBook-, Provider- und Desktop-Abnahme bleibt
> ausdrücklich in `ROADMAP_LINUX.md` offen; dies ist keine Vollparitätsangabe.

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
- Adwaita `ViewStack`, `ToolbarView`, `HeaderBar`, `NavigationSplitView`,
  `OverlaySplitView` und GTK-`Paned`-Container bilden die adaptive
  Evolution-Struktur. Die Mailansicht kann dabei pro Benutzer zwischen einer
  modernen Seitenansicht und der klassischen Evolution-Anordnung mit Ordnern
  links, Nachrichtenliste oben und Vorschau unten umgeschaltet werden.
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
| **Tag/Woche/Monat-Ansicht** | GTK4-rs `AdwViewStack` mit dynamischer Tagesliste, Sieben-Tage-Liste, Arbeitswochen-Zeitachse, Agenda und datumsrichtigem Monatsraster; Navigation und ECal-Signalaktualisierung sind verbunden. Wochenbeginn, Kalenderwochen, Arbeitszeiten, Zeitzone, Datums-/Zeitformat und aktive Standardansicht werden persistent gespeichert und bei Änderungen auch in einer bereits geöffneten Ansicht live synchronisiert. Portable UTC-/TZID-Umrechnung für Anzeige und Tageszuordnung ist angebunden; komplexe DST-/EDS-Abnahme und Drag-and-drop bleiben offen. | `libecal-2.0` über den EDS-Adapter |
| **Event-Erstellung** | `AdwDialog` mit Start/Ende-Picker (`GtkCalendar` + Time-Spinner), Wiederholung, Alarme, Teilnehmer. | `ECalComponent`, `ICalComponent` |
| **ITIP Meeting-Einladungen** | Im Mail-View: Inline-Banner bei `text/calendar` MIME-Parts. Portable Buttons für „Zusagen / Vorbehalt / Ablehnen" erzeugen einen RFC-kompatiblen `METHOD:REPLY`-MIME-Part und persistieren einen Antwortentwurf; tatsächlicher Versand und serverseitiger Teilnehmerstatus werden unter Linux über EDS/Camel abgenommen. | `ECalComponent`, ITIP-Utility-Funktionen |
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
- [~] Flatpak-Manifest reproduzierbar vorbereiten: EDS ist jetzt auf einen
  unveränderlichen Upstream-Commit gepinnt; Generator und statischer Validator
  für `cargo-sources.json` sind vorhanden. Die Cargo-Quellenliste und der
  echte Sandbox-Build bleiben Linux-Abnahme.
- [~] CI-Gates sind durch portable Format-/Schema-/Diff-Prüfungen und die
  Flatpak-Manifest-Validierung vorbereitet; ein vollständiger Linux-Lauf mit
  Clippy, Unit-/Integrationstests und Flatpak-Smoke-Test bleibt offen.

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

- [~] Adaptives GTK4/libadwaita-Mail-Layout; die bestehende GTK4-rs-Mailseite
  wird jetzt mit dem nativen Camel-Service verbunden. Die Ordner-Seitenleiste
  bleibt links, während Benutzer zwischen moderner Liste/Vorschau-
  Seitenansicht und der klassischen Evolution-Anordnung (Liste über der
  Vorschau) wählen können. Beide Modi verwenden denselben Auswahl-,
  Such- und Aktionspfad. Das Ansichtsmenü bietet zusätzlich direkte Umschalter
  für Threading, Avatare, Absender, Datum, Labels, Vorschautext und
  Nachrichtenlistendichte; das Hauptmenü stellt die klassischen Einzel-
  Nachrichtenpfade für Antworten, Weiterleiten, Markieren, Archivieren,
  Spam und Löschen ebenfalls über dieselben Aktionen bereit. Zusätzliche
  native Camel-Abnahme bleibt offen. Die Evolution-kompatiblen Datei-/Ordner-
  aktionen routen jetzt auch „Neuer Ordner“, „Papierkorb leeren“ und „Expunge“
  über die GTK4-rs-Mailseite; lokale app-eigene Papierkörbe werden bestätigt
  geleert, während native EDS/Camel-Expunge weiterhin sichtbar als Linux-
  Backendarbeit ausgewiesen wird.
- [~] Sicherer WebKitGTK-Viewer: JavaScript ist deaktiviert, Navigation wird
  abgefangen und externe Bilder sind standardmäßig blockiert. Unter Linux
  wird der WebKitGTK-Prozess erst beim Öffnen der ersten Nachricht erzeugt,
  damit ein kalter EDS-Start nicht mit der ersten GTK-Oberfläche konkurriert;
  der HTML-Renderer bleibt dabei der produktive Linux-Pfad. Der portable
  Sanitizer entfernt blockierte Void-Tags wie `meta` und `link`, ohne den
  nachfolgenden Mailkörper abzuschneiden; damit bleiben typische HTML-Mails
  im Vorschaufenster lesbar. Der explizite Textmodus wandelt HTML ebenfalls in
  bereinigten Klartext um, statt rohe Tags anzuzeigen; bei leerem HTML wird
  derselbe Klartext-/Vorschau-Fallback verwendet. Die Viewer-Basisfarben und
  die konfigurierten Schriftwerte werden vor den sicheren, eingebetteten
  Mail-CSS-Regeln injiziert; explizite Hintergründe, Farben, Links und
  Tabellenlayouts der Nachricht werden dadurch nicht mehr flächendeckend
  überschrieben. Remote-CSS und Ressourcenzugriffe bleiben aus
  Datenschutzgründen blockiert.
  Die portable Leseeinstellung, ob eine Nachricht beim Auswählen als gelesen
  markiert wird, ist persistent und gilt für lokale sowie native Mailpfade;
  providerabhängige Lesestatus-Synchronisation bleibt Linux/EDS-Abnahme.
- [~] EDS-Source-Registry-Abfrage über D-Bus; die `Data`-Keyfiles werden nach
  echten `Mail Account`-/Kalender-/Adressbuch-Erweiterungen klassifiziert und
  gefundene Mailquellen werden als
  GTK-Modell in der Konten-Seitenleiste angezeigt und Kalender-/Adressbuch-
  quellen werden über eine gemeinsame Quellenklassifikation in den jeweiligen
  Seiten dynamisch aufgelistet. Mail-, Kalender- und Adressbuchansichten
  verwenden dafür dieselbe cachefähige Quellenentdeckung; nicht-sensitive
  letzte Quellenstände werden bei einem EDS-Ausfall ausdrücklich als
  Offline-Cache markiert. Verwaltung und Bearbeitung der EDS-Konten fehlen
  noch.
- [~] Lokaler Mail-Store: vorhandene EML-, Maildir- und mbox-Daten werden in
  einem Worker gelesen; RFC-822-Header, MIME-Text/HTML, dekodierte Anhänge und
  stabile lokale Ordner-IDs werden unterstützt; RFC-2231-Parameter für
  internationale und segmentierte MIME-Dateinamen sowie UTF-8-, ISO-8859-1-
  und Windows-1252-Header/Text werden beim Anhang-/Nachrichtenimport dekodiert.
  Camel-Indizes, vollständige
  MIME-Semantik und serverseitige Synchronisation fehlen noch; der lokale
  Import bleibt gegenüber vorhandenen Quellen bewusst nicht überschreibend.
  EML-/mbox-Dateien und ausgewählte Maildir-Verzeichnisse können in den
  konfigurierten lokalen Ordner kopiert werden; `cur`/`new`-Strukturen und
  Maildir-Seen-Flags sowie verschachtelte lokale Evolution-Ordner bleiben beim
  portablen Profilimport erhalten, Wiederholungen erzeugen nummerierte
  Dateinamen. Der automatisch erkannte Evolution-Mailstore bleibt dabei eine
  read-only Quelle; direkte Datei-/Ordnerimporte werden in den app-eigenen
  Mailbestand umgeleitet und der Zielpfad persistent gesetzt, damit weder
  Originaldateien noch Camel-Indizes verändert werden. Benutzerdefinierte
  importierte Ordner werden zusätzlich im
  app-eigenen Sidecar registriert, damit auch ein Ordner ohne Nachricht nicht
  aus der Sidebar verschwindet; leere Mailbox-Dateien und erkannte
  Maildir-/`.sbd`-Ordner werden dabei ebenfalls registriert. Der lokale
  Mailordner kann über die
  Einstellungen ausgewählt oder auf den automatisch erkannten Pfad
  zurückgesetzt werden. Ein app-eigener Sidecar speichert zusätzlich logische
  Ordnerübersteuerungen, lokale Mehrfachkopien und gelöschte Einträge, ohne die
  Originaldateien zu verändern. Bereits importierte benutzerdefinierte
  `custom:…`-Ordner werden in der portablen GTK4-rs-Mailseitenleiste ebenfalls
  dynamisch angezeigt und filterbar gemacht. App-eigene leere lokale Ordner
  können erstellt, umbenannt und gelöscht werden; beim Umbenennen werden
  logische Sidecar-Platzierungen mitgeführt, und das Löschen nicht leerer
  Ordner wird sicher abgelehnt. Benutzerdefinierte lokale Ordner stehen auch
  als reversible Ziele für Verschiebeaktionen zur Verfügung. Eine
  providerunabhängige logische Ordnerhierarchie ergänzt fehlende Eltern,
  aggregiert Zähler, unterstützt Ein-/Ausklappen und führt Umbenennen/Löschen
  auf dem gesamten Unterbaum aus; native providerabhängige Hierarchie- und
  Separator-Semantik bleibt Linux-/Camel-Abnahme. Beim initialen Laden bleiben
  diese lokalen Nachrichten und Ordner auch bei vorhandenen EDS-Konten als
  eigener „Auf diesem Computer“-Bestand neben den nativen Konten sichtbar;
  paginierte native Ordnerabfragen bleiben davon getrennt.
- [~] Suche, Stern, Wichtigkeitsstatus, Ordnerfilter und „gelesen“ existieren
  im GTK4-rs-UI;
  die lokale Schnellsuche versteht zusätzlich `from:`, `to:`, `subject:`,
  `cc:`, `bcc:`, `body:`, `filename:`, `in:`/`folder:`, `on:`, `after:`,
  `before:`, `older:`, `newer:`, `is:unread`, `is:read`, `is:starred`,
  `is:important`, `is:not-important`,
  `is:sent`/`is:junk` und `has:attachment`/`no:attachment` und
  durchsucht auch Empfänger, Vorschau, HTML-Inhalt und Dateinamen;
  Mail- und Thread-Datumsanzeigen verwenden die persistenten Kalender-
  Datums-/Zeitformate;
  Detailauswahl sowie Lesen, Markieren und Löschen werden für Remote-Nachrichten
  aus dem Servicepfad in diese Oberfläche zurückgeführt. Für den lokalen Import
  werden Zustände in einem app-eigenen Sidecar persistiert; Remote-Flags werden
  über Camel/EDS gelesen und zurückgeschrieben. Die lokale Wichtigkeits-
  markierung wird separat im Sidecar gespeichert; die providerabhängige
  Remote-Wichtigkeitssemantik bleibt bis zur Linux/Camel-Abnahme ausdrücklich
  offen. Mehrfachauswahl mit
  Gelesen-, Wichtigkeits-, Archiv-, Spam-, Lösch- und Verschiebeaktionen ist
  angebunden; auch die
  Einzel-Nachrichtenansicht bietet Verschieben über denselben reversiblen
  Command-/Undo-Pfad. Für lokale/app-eigene Nachrichten ist zusätzlich
  „In Ordner kopieren“ mit persistentem Sidecar, Batch-Aktion und Undo/Redo
  vorhanden. Microsoft-Graph-Nachrichten werden serverseitig in den gewählten
  Ordner kopiert; die von Graph vergebene neue Nachrichten-ID wird im
  Command-/Undo-Verlauf gespeichert, sodass Undo/Redo nur die Kopie betrifft.
  Native EDS/Camel-Kopien bleiben bis zur Linux-Abnahme offen.
  Die Ziele werden aus der dynamischen EDS-Ordnerliste
  angeboten und für lokale Nachrichten aus der app-eigenen Ordnerhierarchie
  ergänzt; alle sichtbaren Treffer können per Menü oder `Ctrl+A` für diese
  Batch-Aktionen ausgewählt werden. Für Microsoft Graph ist die
  serverseitige Mail-Suche für positive Volltext-, Absender-, Empfänger-,
  Betreff-, Text-, Anhangs- und Anhangsstatus-Prädikate asynchron, debounced,
  abbrechbar und paginiert an denselben Mail-/Threadpfad angebunden; negative
  Prädikate sowie Status-, Label-, Ordner- und Datumsfeinheiten bleiben als
  lokaler Nachfilter erhalten. Native Camel-Suche, CamelVee-Suchordner und
  native EDS/Camel-Kopien und providerabhängige Live-Zähler bleiben
  Linux-/Backend-Abnahme. Der Nachrichtentext wird vor der Darstellung
  sicher bereinigt; wenn defektes oder vollständig blockiertes HTML keinen
  sichtbaren Inhalt ergibt, zeigt der GTK4-rs-Viewer den verfügbaren
  Klartext-/Vorschau-Auszug statt einer leeren weißen Fläche.
- [~] Der Startordner der Mailansicht ist als portable Einstellung mit
  Posteingang, Alle Nachrichten, Gesendet, Entwürfe, Papierkorb, Archiv und
  Spam konfigurierbar und wird in Backup/Restore sowie beim lokalen Fallback
  berücksichtigt. Änderungen an der Einstellung werden auch in einer bereits
  geöffneten Mailansicht sofort auf die statische Auswahl und den aktuell
  gewählten EDS-Konto-Ladepfad angewendet; providerabhängige Alias- und
  Ordnerauflösung bleibt Teil der Linux/Camel-Strecke.
- [~] Die Mail-Liste kann portabel nach Datum, Betreff, Absender und
  Wichtigkeitsstatus sowie
  auf-/absteigend sortiert werden; Feld, Richtung und das Gruppieren von
  Unterhaltungen werden über GSettings gespeichert und beim Wiederaufbau der
  Liste angewendet; neben Datum, Betreff und Absender stehen auch Ungelesen-
  Status, Markierung, Wichtigkeit und Anhänge als Sortierfelder zur Verfügung; die
  portable Datumssortierung normalisiert RFC-2822-/RFC-3339-Werte statt rohe
  Headerstrings zu vergleichen und sortiert Betreff/Absender ohne
  Groß-/Kleinschreibungsartefakte. Änderungen an diesen Einstellungen aktualisieren eine
  bereits geöffnete Ansicht ebenfalls. Vorschaufenster und die Dichte der
  Nachrichtenzeilen sowie die Mail-Arbeitsweise (modern nebeneinander oder
  klassisch Liste über Vorschau) werden ebenfalls persistent gespeichert und
  bei einem Einstellungswechsel sofort auf die geöffnete GTK4-rs-Ansicht
  angewendet. Die klassische Evolution-Drei-Bereiche-Ansicht (Ordner links,
  Nachrichtenliste oben, Vorschau unten) ist dabei der portable Standard; die
  moderne Anordnung bleibt als persönliche Einstellung verfügbar. Die
  Sichtbarkeit und Breite der Ordner-Seitenleiste sowie die
  Trennbalkenpositionen der modernen und klassischen Ansicht werden separat
  gespeichert, auch in Backup/Restore
  übernommen und durch responsive automatische Sidebar-Ausblendungen nicht
  überschrieben. Absender, Empfänger, Avatare, Datum, Anhangssymbol,
  Vorschautext und app-eigene Labels der Mail-Zeile lassen sich ebenfalls
  einzeln ein-/ausblenden und werden live sowie in Backup/Restore angewendet.
  Eine kompakte Evolution-Kopfzeile benennt die sichtbaren Spalten und bietet
  am Absenderfeld einen ziehbaren Breiten-Griff; die Breite bleibt mit dem
  Spaltenmenü persistent. Betreff, Absender, Datum und Anhänge sind in der
  Kopfzeile direkt anklickbare Sortierfelder und zeigen Richtungspfeile.
  Der helle Mail-Reader färbt `html` und `body`
  konsistent, blendet blockierte externe Bild-Platzhalter aus und lässt
  bekannte eingebettete `cid:`-/`data:`-Rasterbilder weiterhin sichtbar.
  Der Datenschutz-Banner erscheint nur, wenn der konkrete Nachrichtentext
  tatsächlich externe Bildquellen enthält, nicht bei reinen Inline-Bildern
  oder textbasierten Nachrichten. Der gemeinsame Klartextpfad für Vorschau,
  Suche und Fallback decodiert zusätzlich benannte sowie dezimale/hexadezimale
  HTML-Entities, damit internationale Mailtexte nicht als Entity-Namen
  erscheinen.
  Eine providerunabhängige
  Datumsnormalisierung und serverseitige Sortierung bleiben Teil der
  Linux/Camel-Strecke. Die Zeilen bleiben dabei tatsächlich einzeilig:
  Betreff und Vorschautext nutzen den flexiblen Mittelbereich, Anhangs-,
  Markierungs- und Wichtigkeitssymbole sitzen kompakt am rechten Rand und
  die nicht benötigten Auswahlkästchen werden nicht mehr angezeigt. Die
  Konversationsspur im Reader ist als scrollbar begrenzter Kurzbereich mit
  einer sichtbaren Zeile ausgeführt; die Detailkopfzeile lässt dem eigentlichen
  Nachrichteninhalt entsprechend mehr Raum. Mail-CSS wird über einen
  restriktiven, ressourcenfreien
  Sanitizer übernommen; externe Bild-Elemente werden vor der WebKit-
  Darstellung entfernt und erst nach ausdrücklicher Freigabe aus dem
  gespeicherten Originalinhalt erneut sanitisiert und geladen. Die Freigabe
  gilt nur für die aktuelle Nachricht, wird beim Nachrichtenwechsel
  zurückgesetzt und kann für Nachrichten persistent gespeichert werden.
  Konten-, Kalender-, Adressbuch-, Aufgaben- und Ordnerstatus bleiben in der
  sichtbaren Oberfläche technisch neutral beschriftet; die Konten-/Ordnerleiste
  zeigt Konten als aufklappbare Wurzeln mit ihren Standard- und Unterordnern in
  kompakter Dichte. Die separate Kontenliste bleibt für die Auswahl- und
  Ladekoordination intern, erzeugt aber keine doppelte sichtbare Navigation.
  Die Nachrichtenzeilen enthalten keine permanenten Auswahlkästchen mehr;
  sichtbare Einzelaktionen bleiben am rechten Rand und Mehrfachaktionen sind
  weiterhin über Auswahlbefehle und `Ctrl+A` erreichbar.
  Wiederverwendete GTK-Listenzeilen binden Markierungs- und Wichtigkeitsaktionen
  nur einmal an die Zeile und lesen beim Klick den aktuell gebundenen Datensatz,
  damit bei großen Ordnern keine veralteten Mehrfach-Callbacks entstehen.
- [~] Die Evolution-kompatible Mail-Werkzeugleiste ist standardmäßig sichtbar,
  sitzt über dem gesamten Mailbereich (Nachrichtenliste und Vorschau) und kann
  portabel ein- und ausgeblendet werden. Ihre vertrauten Aktionen
  verwenden dieselben zentralen
  Menü-, Detail- und Command-/Undo-Pfade; zusätzliche providerabhängige
  Camel-Aktionen bleiben bis zur Linux-Abnahme offen.
- [~] Anhang „Speichern unter“ ist für lokal gelesene MIME-Anhänge angebunden;
  ausgewählte Nachrichten können zusätzlich als vollständige EML/RFC822-Datei
  mit Thread-Headern und MIME-Anhängen exportiert werden. Lokale MIME-
  Content-IDs werden für bekannte Rasterbilder sicher und größenbegrenzt in
  die HTML-Darstellung eingebettet; EDS-/Remote-Anhangsdaten und die
  Linux-Laufzeitprüfung bleiben offen.
- [~] Asynchrones Laden lokaler Nachrichten ohne Blockierung des GTK-Main-
  Threads mit Abbruchsignal ist vorhanden; die begrenzten Camel-Seiten werden
  ebenfalls über den Worker in die GTK4-rs-Mailseite geladen. Camel-Pagination,
  deduplizierte Aktualisierung, Refresh, account-spezifische Ordnerauswahl,
  GSettings-gesteuertes Hintergrund-Polling, native Camel-
  Ordneränderungssignale und detaillierte Konto-/Ordner-/Nachrichten-
  Fortschrittsmeldungen sind angebunden. Cancellation innerhalb laufender
  Camel-Aufrufe, UID-basierte inkrementelle Nachrichtenaktualisierung und
  begrenztes Retry/Backoff für Mail-Ladevorgänge sind ebenfalls angebunden.
  Der sichtbare Quellenstatus bleibt bis zum tatsächlichen Backend- oder
  lokalen Fallback-Ergebnis erhalten und unterscheidet jetzt auch ein
  erkanntes Konto ohne lesbare Mailordner von einem erfolgreichen Laden.
  Der Camel-Änderungsmonitor startet erst nach dem ersten sichtbaren
  Ladeergebnis, damit ein kalter EDS-Start nicht gleichzeitig Voll-Load und
  Dauer-Watcher initialisieren muss; nicht sichtbare EDS-Module werden beim
  ersten Öffnen lazy erzeugt. Eine vollständige inkrementelle
  Ordner-/Kontosynchronisation und eine einheitliche Retry-/Netzwerkstrategie
  für alle EDS-Domänen fehlen noch. Der GTK-Modellaufbau ist zusätzlich in
  32er-Hauptschleifen-Chunks aufgeteilt: Nachrichten, Threadgruppen und
  Threadzeilen werden nach jedem Chunk zurück an GTK übergeben, damit große
  EDS-/lokale Mailboxen den Start nicht mehr als „Programm reagiert nicht“
  erscheinen lassen. Der erste Mail-Worker wird erst nach dem Präsentieren
  des Fensters gestartet; die EDS-Quellenerkennung läuft zusätzlich in einem
  eigenen Worker-Kontext und aktualisiert GTK nur mit fertigen, einfachen
  Daten. Die Startdiagnose protokolliert die Zeitpunkte von Fensterpräsentation,
  Backend-Rückkehr, Quellenentdeckung und Modellabschluss; die tatsächliche
  Linux-EDS-/Camel-Abnahme bleibt in [ROADMAP_LINUX.md](ROADMAP_LINUX.md).
  Die automatische Markierung einer ausgewählten Nachricht als gelesen läuft
  jetzt ebenfalls vollständig im Worker; der GTK-Auswahlcallback wartet nicht
  mehr synchron auf Camel/EDS-Authentifizierung oder Ordnersynchronisation.
- [~] Native Linux-Camel-Brücke öffnet die aktivierten EDS-Mailquellen über
  `e_source_camel_configure_service()`, verwendet EDS-/GOA-Credentials und
  lädt Ordnerzusammenfassungen sowie begrenzte RFC-822-Nachrichten in
  thread-sichere Servicewerte. Die GTK4-rs-Mailseite zeigt die initial geladenen
  Nachrichten, Status-/Ordnerzähler und übernimmt Unread-/Flagged-Camel-Flags;
  Lesen, Markieren und Löschen werden über stabile EDS-/Camel-Referenzen
  zurückgeschrieben. Begrenzte Folge-Seiten, Refresh, Kontoauswahl und
  account-spezifische Camel-Ordnerauswahl werden über denselben Servicepfad
  geladen. Der lokale EML/Maildir/mbox-Importer bleibt der explizite Fallback.
  Die GTK4-rs-Seitenleiste bildet die geladenen EDS-Ordner einschließlich
  Unterordnern als aufklappbares TreeListModel mit stabilen Konto-/Ordner-
  Schlüsseln ab; providerabhängige Hierarchie-Separatoren und strukturelle
  Lebenszyklusänderungen werden noch auf Linux verifiziert.
  Die native Konfiguration übernimmt die vollständige ESource-Elternkette
  sowie geerbte GOA-/OAuth2- und EWS-Identitätsdaten; ein erfolgreicher
  Kontozugriff ohne lesbaren Ordner wird im Status explizit als solcher
  ausgewiesen. Der echte Provider-/Linux-Laufzeittest bleibt offen.
  Archivieren, Verschieben und Spam-/Nicht-Spam-Markierung werden über denselben
  Servicepfad ausgeführt; Camel-Änderungs-, Lösch- und Umbenennungssignale
  werden über einen langlebigen nativen Watcher in die Mailansicht geleitet.
  Serverzustände werden nach Mutationen verifiziert und als explizite
  Konflikte zurückgegeben; betroffene Nachrichten werden über ihre Camel-UIDs
  inkrementell nachgeladen oder entfernt. RFC-822-Thread-Identitäten,
  transitive Antwortketten, ein anklickbarer Konversationskontext und eine
  aufklappbare GTK4-rs-TreeListModel-Darstellung sind für lokale und partiell
  geladene Camel-Nachrichten angebunden; die servernahe CamelFolderThread-
  Prüfung, vollständiges Nachladen aller Thread-Mitglieder, strukturelle
  Ordnersynchronisation und der Linux-Laufzeittest mit einem realen GNOME-
  Konto stehen noch aus.
- [~] Mailaktionen besitzen eine zentrale Backend-Schnittstelle. Lokale
  Nachrichten behalten den schreibgeschützten Import mit Sidecar für
  Lesestatus/Markierung/Wichtigkeit; lokale Archivieren-, Verschieben-,
  Wichtigkeits- und Löschen-Aktionen
  werden ebenfalls nur im app-eigenen Sidecar persistiert und sind über den
  Command-Stack rückgängig machbar. Remote-Nachrichten verwenden Camel für
  Lesen, Flags, Archivieren, Verschieben, Spam-/Nicht-Spam-Markierung und
  Löschen. Batch-Aktionen und
  der zentrale Aktionspfad werden aus dem Servicepfad in die GTK4-rs-Mailansicht
  übernommen. Mehrfachauswahl für Gelesen/Ungelesen, Markierung, Wichtigkeit,
  Archivieren,
  Spam/Nicht-Spam und Löschen sowie
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
  Fehlerzustand behandelt und Batch-Fehler im sichtbaren Mailstatus angezeigt.
  Der Detailkopf bietet für eine einzelne Auswahl ebenfalls direkte
  Markierungs-, Archiv- und Spam-/Nicht-Spam-Aktionen über denselben
  Command-/Undo-Pfad.
- [~] Portable virtuelle Suchordner sind vorhanden: vier Standardabfragen und
  beliebig viele gespeicherte lokale Suchdefinitionen können angelegt,
  bearbeitet und gelöscht werden. Die vorhandene lokale Suchsprache wird
  ausgewertet; Suchordner zeigen Treffer-/Ungelesen-Zähler für den geladenen
  Nachrichtenbestand. Der portable Graph-Pfad führt kompatible positive
  Suchbestandteile bereits über Outlook-KQL auf dem Server aus und wendet die
  übrigen Prädikate lokal nach; serverseitige gespeicherte Suchordner,
  CamelVeeStore/CamelVeeFolder und providerübergreifende Live-Zähler bleiben
  Linux-/Backend-Arbeit.

#### Phase 2 — Konten, Empfang und Mailversand

- [~] Konto-Setup und Composer sind vorhanden; der GTK4-rs-Composer kann
  Entwürfe ohne Kontogeheimnisse in einem app-eigenen Store persistieren und
  der Start zeigt gespeicherte EDS-Mailkonten. EDS-Identität und der native
  Camel-Transport sowie das Einhängen in EDS-Drafts/Sent sind jetzt als
  Versandpfad angebunden. Die portable lokale Entwurfsverwaltung kann
  Entwürfe auflisten, öffnen, aktualisieren, löschen und nach erfolgreichem
  Versand bereinigen; lokale Kontoprofile können bearbeitet, aktiviert und
  entfernt werden. Wenn EDS nicht erreichbar ist, werden aktivierte lokale
  Kontoprofile samt Alias-Adressen im Composer als ausdrücklich lokaler
  Absender angeboten; ein Versandversuch bleibt bis zur nativen Camel-
  Aktivierung ein sichtbarer Backendfehler. Die portable Profilvalidierung
  prüft E-Mail-, Alias-, Server-, Port- und TLS-Werte vor dem Speichern; EDS-
  Kontoänderungen und die Linux-Laufzeitabnahme fehlen noch. Der Kontowizard
  blendet protokollabhängige Server-, Ordner- und Zugangsfelder jetzt für
  Exchange bzw. GOA nachvollziehbar aus oder ein.
- [~] Sichtbare Lese-/Datenschutz-/PGP-Optionen werden über GSettings
  persistent gespeichert; direkte Cargo-Entwicklungsstarts verwenden eine
  automatisch kompilierte Schema-Kopie, falls noch keine Meson-Installation
  vorhanden ist. Vorschaufenster,
  Markieren-beim-Auswählen, HTML/Text, externe Bilder, Schriftfamilie/-größe,
  Dark-Mode und Bestätigungsdialoge werden auch auf laufenden Ansichten
  berücksichtigt. Der geöffnete MailViewer speichert den aktuellen Inhalt
  für eine sichere Neudarstellung und übernimmt diese Leseeinstellungen
  sofort, ohne die Nachricht neu auswählen zu müssen.
- [~] EDS-Quellen dynamisch als Konten anzeigen; das Setup speichert zusätzlich
  nicht-geheime Kontoprofile und kennzeichnet sie bis zur EDS/Camel-Aktivierung
  als ausstehend. Lokale Profile lassen sich bearbeiten, deaktivieren und
  entfernen; Alias-Adressen und die nicht-geheime Ordnerzuordnung für
  Posteingang, Entwürfe, Gesendet, Archiv, Papierkorb und Junk werden portabel
  gespeichert und aus Evolution-Profilen übernommen. Standardkonto und
  Standard-ECal-Quelle werden portabel über GSettings vorselektiert und in
  Backup/Restore übernommen. EDS-Verknüpfung,
  Secret-Service-Aktivierung und die native Anwendung der Zuordnung fehlen
  noch. Die gespeicherte Standardkonto-ID wird beim Mailstart jetzt auch auf
  die passende live entdeckte oder cachefähige EDS-Quelle angewendet; noch
  nicht aktivierte lokale Profile bleiben dabei bewusst nicht auswählbar.
- [~] IMAP/IMAPX, POP3, lokale Ordner, SMTP und Exchange/Microsoft-365 mit
  OAuth2, TLS, Passwort-/Secret-Service und Offline-Fehlerzuständen. Der
  portable Microsoft-Graph-Client ist für typisierte Fehler, geheimnisfreie
  Token-Injektion, GAL-Suche, OOF, Ordnerhierarchie/Pagination, Mail-Lesen
  einschließlich Graph-Anhängen, MIME-Versand/Entwürfe, Zustände/Labels/
  Verschieben/Kopieren, Kalenderansicht mit CRUD und Grundserien sowie Kontakte mit
  CRUD implementiert und in die bestehenden Mail-, ECal- und EBook-Verträge
  überführt. Die GTK4-rs-UI fällt bei einem passenden Exchange-Profil auf
  diesen Pfad zurück. Interaktive OAuth2-/EDS-/GOA-Tokenbeschaffung, Delta-
  Synchronisation, vollständige Graph-Anhangstypen, komplexe Zeitzonen/
  Serien-/Meeting-Semantik sowie IMAP/POP3/SMTP bleiben offen und müssen auf
  Linux mit einem echten Konto abgenommen werden.
- [~] GTK4-rs-Composer zeigt aktive EDS-Mailkonten, persistiert To/Cc/Bcc,
  Anhänge sowie HTML- und Nur-Text-Entwürfe, kann Entwürfe wieder aufnehmen und erzeugt
  Reply-/Forward-Vorbelegungen. Reply-All nutzt jetzt Reply-To, To und Cc,
  dedupliziert Empfänger und übernimmt den Originaltext als sicheres Zitat;
  Weiterleiten übernimmt die Originalkopfzeilen und den Nachrichtentext. Ein
  getesteter RFC822/MIME-Builder und die native CamelTransport-Aufrufstrecke
  sowie EDS-Drafts/Sent-Fallback sind angeschlossen; die Composer-Quellenliste
  verwendet ebenfalls die live-/cachefähige EDS-Entdeckung und kennzeichnet
  importierte Offline-Quellen ausdrücklich. Lokale
  aktivierte Profile werden einschließlich ihrer Alias-Adressen als getrennte
  auswählbare Von-Identitäten angeboten; `From` und `Reply-To` werden für den
  portablen RFC822-Versand und lokale Entwürfe aus der gewählten Identität
  übernommen, und ein wiedergeöffneter Entwurf stellt die passende Alias-
  Identität wieder her. Lokale
  Entwurfsverwaltung und konfigurierbares lokales Autosave sind ebenfalls
  angeschlossen; Änderungen am Autosave-Intervall ersetzen den laufenden
  Timer auch in einem bereits geöffneten Composer. Eine lokale
  Klartext-Standardsignatur wird über GSettings
  verwaltet und in neue, beantwortete und weitergeleitete Nachrichten
  eingefügt. PGP-Signieren/Verschlüsseln und die native
  EDS-Draft-Lebenszyklusprüfung bleiben offen. Das Zitat-Ein/Aus und
  Blockzitat- bzw. `>`-Präfixformat sind persistent konfigurierbar. Der
  Composer kann app-eigene HTML-/Betreff-Vorlagen sowie Textbausteine mit
  optionalem Kürzel speichern, an der Cursorposition einfügen und löschen;
  beide Speicher sind versioniert und in Backup/Restore enthalten.
  To/Cc/Bcc bieten zusätzlich eine getrennte, asynchrone portable
  Exchange-GAL-Verzeichnissuche mit Treffer-Popover, Pagination, Timeout und
  sichtbarer Fehlerbehandlung; lokale vCard-Treffer und serverseitige
  Verzeichnistreffer bleiben dabei getrennte Quellen.
  Der Composer bietet portabel Fett/Kursiv/Unterstrichen, Aufzählungen,
  Nummernlisten, Blockzitate sowie Undo/Redo; der Nur-Text-Modus wird als
  `text/plain` ausgegeben und die Standardwahl wird über GSettings gespeichert.
  Mail-/Composer-Schriftfamilie und -größe werden persistent angewendet, und
  die WebKit-Rechtschreibprüfung ist aktiviert. Die portable Auswahl von
  Systemstandard, Deutsch, Englisch oder beiden Wörterbuchsprachen wird
  gespeichert, in Backup/Restore übernommen und auf neue Composer angewendet;
  die tatsächlich installierten Wörterbücher bleiben hostabhängig.
  Die portable Lesebestätigungs-Standardeinstellung wird ebenfalls persistent
  gespeichert, in Backup/Restore übernommen und beim RFC822-Versand als
  `Disposition-Notification-To` ausgegeben; SMTP-DSN, providerabhängige
  Zustellberichte und die native EDS/Camel-Anwendung bleiben Linux-only.
- [~] Synchronisationsstatus, Cancellation und begrenztes Retry/Backoff sind
  für den nativen Mail-Ladepfad vorhanden; Netzwerkwechsel, domänenweite
  Offline-/Online-Zustände, vollständige Wiederaufnahme und sauberes
  Herunterfahren fehlen noch.

#### Phase 3 — Kalender, Aufgaben und Memos

- [~] Monatsansicht, Navigation, View-Switcher sowie echte Tages- und
  Wochenlisten sind vorhanden;
  die GTK4-rs-Kalenderansicht berechnet den Monatsraster datumsrichtig,
  markiert den heutigen Tag, zeigt geladene VEVENTs direkt in den Tageszellen
  und berücksichtigt in Tag/Woche auch zeit- und ganztägige Mehrtagstermine.
  Standardansicht, Standard-ECal-Quelle, Wochenbeginn, ISO-Kalenderwochen,
  Datums-/Zeitformat, auswählbare System-/UTC-/IANA-Kalenderzeitzone,
  Arbeitstage, Arbeitszeit und die Standard-Erinnerung werden über
  GSettings persistent konfiguriert und in
  der portablen Ansicht bzw. im Termin-Editor verwendet.
  EDS-Quellenstatus wird angezeigt und neue VEVENTs können über den nativen
  ECal-Adapter gespeichert, bearbeitet und gelöscht werden; iCalendar-Export,
  Wiederholungsregeln, Teilnehmer, Kategorien, private Termine, Status/Verfügbarkeit,
  Konferenz-URLs, einfache Erinnerungen und portable Datei-/URI-Anhänge sind im Editor
  vorbereitet.
  Beim Bearbeiten bleiben bestehende `ATTACH`-Parameter für unveränderte URIs erhalten;
  eine Dateiauswahl ergänzt mehrere Anhänge zeilenweise. Zusätzliche Vorkommen und
  Ausnahmen werden über `RDATE`/`EXDATE` kommasepariert bearbeitet und behalten bei
  vorhandenen Einträgen die DATE-/Zeitzonen-Parameter. Kategoriebezeichnungen werden
  zusätzlich in einem portablen, versionierten Farbspeicher verwaltet und
  können in den Einstellungen bearbeitet werden. Lokale/importierte VEVENTs werden
  zusätzlich in allen Kalenderansichten dynamisch nach RFC5545-CATEGORIES
  filterbar; konfigurierte sowie im geladenen Bestand vorhandene Kategorien
  werden gemeinsam angeboten. Änderungen an Kategorien und ihren app-eigenen
  Farben aktualisieren die bereits geöffneten Kalender-, Aufgaben- und
  Notizzeilen einschließlich ihrer Filtermodelle sofort. Eine portable
  Termin-Schnellsuche durchsucht
  Betreff, Ort, Beschreibung, Zeitraum und Kalenderquelle und wirkt in allen
  Kalenderansichten sowie der Terminliste. Lokale/importierte VEVENTs werden
  in den Tag-, Wochen- und Monatsansichten nach DAILY/WEEKLY/MONTHLY/YEARLY-
  RRULE sowie COUNT/UNTIL/EXDATE ausgewertet. BYDAY mit Wochentagslisten und
  ordinalen Monatswochen, BYMONTH, positive/negative BYMONTHDAY und BYSETPOS
  werden portabel berücksichtigt. Eine portable Agenda zeigt die
  kommenden 90 Tage und berücksichtigt mehrtägige/wiederkehrende VEVENTs;
  Arbeitswochenraster und die Nutzung der Arbeitszeit in einer Zeitachse sind
  jetzt als portabler Arbeitswochen-View mit Ganztagesterminzeile und
  zeitlich einsortierten Terminen vorhanden; Terminzeilen und die
  Monatstermin-Chips öffnen den Bearbeitungsdialog, sofern die Quelle
  schreibbar ist. Ein portables Drag-and-drop verschiebt einfache Einzeltermine
  in Monats- und Arbeitswochenansicht und erhält dabei vorhandene
  Zeitzonenparameter; das Verschieben einer geladenen Serie verschiebt portabel
  DTSTART/DTEND, RDATE/EXDATE und ein vorhandenes RRULE-UNTIL gemeinsam. UTC-
  und benannte `TZID`-Werte werden für Anzeige, Tageszuordnung und einfache
  Erinnerungszeitpunkte portabel in die lokale Zeitzone umgerechnet; komplexe
  Serien-DST-Umrechnung und die Linux-Abnahme bleiben offen. Der
  app-eigene lokale Kalender kann außerhalb von EDS VEVENTs persistent
  erstellen, ändern und löschen; ausdrücklich ausgewählte externe Importordner
  bleiben schreibgeschützt. Ein portabler Reminder-Service liest lokale
  `VALARM`-Display-Trigger aus VEVENT/VTODO, speichert Zustellung und Snooze
  atomar und bietet 10-Minuten-/1-Stunden-Aktionen über die App-Notification;
  Desktop-Benachrichtigungen lassen sich persistent deaktivieren und werden
  vom Scheduler respektiert;
  die native EDS-Quelle, komplexe Wiederholungs-/Zeitzonenfälle und die
  Linux-Laufzeitabnahme bleiben offen. Beim portablen Terminbearbeiten bleiben
  bestehende ATTENDEE-Parameter sowie nicht unterstützte VALARM-Typen erhalten;
  einfache Display-Alarme können separat geändert oder entfernt werden. Die
  portable Kalenderansicht bietet zusätzlich eine lokale Freie-Zeiten-Abfrage
  innerhalb der konfigurierten Arbeitszeit, die überlappende Termine zusammen-
  führt und eine Mindestdauer berücksichtigt; native EDS-Free/Busy-Daten und
  Teilnehmerverfügbarkeit bleiben Linux-only. Die Kalenderansicht listet EDS-
  und lokale Kalenderquellen mit persistenten Sichtbarkeitsschaltern; ausge-
  blendete Quellen werden in Monats-, Tages- und Wochenansicht berücksichtigt.
  Ein app-eigener `ical`-Ordner bleibt nach dem Speichern seines Pfads
  weiterhin schreibbar; ausdrücklich externe Kalenderpfade bleiben dagegen
  read-only.
- [~] Änderungen an Kalenderansicht, Wochenbeginn, Kalenderwochen,
  Arbeitstagen/-zeiten und ausgeblendeten Quellen werden auch bei geöffneter
  Kalenderseite live aus GSettings übernommen; die aktive View-Schaltfläche
  und die Kalenderwochenanzeige bleiben dabei synchron. Die native EDS-
  Laufzeitabnahme der Quelle bleibt Linux-only.
- [~] GTK4-rs-Aufgaben- und Memo-Ansichten laden VTODO-/VJOURNAL-Komponenten
  über die native libecal-2.0-Brücke aus den aktivierten EDS-Quellen. Neue
  VTODOs/VJOURNALs, VTODO-Statusänderungen und vollständiges VTODO-Bearbeiten
  werden persistent über EDS geschrieben. Der lokale iCalendar-Importer bleibt
  ein expliziter Fallback und sein Quellordner ist über die Einstellungen
  auswählbar; inkrementelle ECal-Signale sind für die drei
  Listenmodelle angebunden. Die Kalenderseitenleiste zeigt die geladenen
  Aufgaben jetzt ebenfalls mit Fälligkeit, Priorität und Status; der
  Erledigt-Schalter schreibt den Status über denselben lokalen bzw. nativen
  ECal-Mutationspfad zurück. Aufgaben- und Notizseiten zeigen ihre
  verfügbaren EDS-/lokalen Listen zusätzlich als synchronisierte linke
  Navigation. Eine vollständige Aufgabenlisten-Parität fehlt noch. Wenn EDS
  nicht verfügbar ist, können neue lokale VTODOs und
  VJOURNALs im app-eigenen Kalenderbestand persistent gespeichert werden.
  Standard-Aufgabenliste und Standard-Notizliste werden getrennt von der
  Standard-Kalenderquelle in den Einstellungen vorselektiert, als lokale oder
  EDS-Quelle angezeigt und in Backup/Restore übernommen. Aufgaben- und
  Notizlisten können zusätzlich über eine persistente Quellen-Auswahl gefiltert
  werden; nicht mehr verfügbare gespeicherte Quellen bleiben als Hinweis
  sichtbar.
- [~] EDS-Source-Registry und ESource-basierte Kalenderauswahl werden dynamisch
  angezeigt; Kalender-, Aufgaben- und Memo-Quellen werden per
  e_cal_client_connect_sync() gelesen, inklusive Quellidentität und
  Backend-Warnungen. Der native Adapter liefert bereits Create/Modify/Remove-
  Operationen mit Konfliktstatus; UI-seitige Bearbeiten-/Löschen-Flows sind für
  Termine, Aufgaben und Notizen angebunden. Eine vollständige
  Konfliktauflösung fehlt noch; die ECal-Signalaktualisierung ist umgesetzt.
- [~] Tag-/Woche-/Monat-Ansichten mit echten VEVENTs sind im GTK4-rs-Frontend
  vorhanden; Wochenbeginn und ISO-Kalenderwochen sind konfigurierbar. Die
  portable Agenda und die Arbeitswochen-Zeitachse sind als eigene, persistente
  View-Modi ergänzt; die Zeitachse nutzt die konfigurierbare Arbeitszeit und
  trennt Ganztages- von zeitgebundenen Terminen. Einfaches Drag-and-drop für
  Einzeltermine und geladene Serien ist in Monats- und Arbeitswochenansicht
  vorhanden; die portable UTC-/TZID-Umrechnung ist für Anzeige und
  Tageszuordnung vorhanden, die Serien-DST-Umrechnung und Linux-Laufzeitprüfung
  stehen noch aus.
- [~] Lokale VEVENTs unterstützen Wiederholungen und einfache Alarme; `RDATE`-
  Einzeltermine werden zusätzlich zu `RRULE` und `EXDATE` in den portablen
  Kalenderansichten und lokalen Erinnerungen ausgewertet. Der VTODO-Editor
  persistiert einfache Display-Alarme und Kategorien. `text/calendar`-
  Nachrichten werden portabel als iTIP-Einladung erkannt; REQUEST/PUBLISH/ADD
  kann in den app-eigenen lokalen Kalender übernommen werden. Portable
  ACCEPTED/TENTATIVE/DECLINED-Antworten erzeugen bereits einen RFC-kompatiblen
  `METHOD:REPLY`-Kalenderteil und einen persistierten app-eigenen
  Antwortentwurf. Komplexe freie-Zeiten-/DST-Fälle, komplexe Alarmtypen sowie native
  EDS-/Camel-Anhangsübertragung und Offline-Sync fehlen noch; der
  serverseitige Teilnehmerstatus bleiben für die Linux-Abnahme zuständig. Der portable Reminder-Service wertet lokale
  `VALARM`-Display-Trigger aus und persistiert Zustellung/Snooze; die native
  EDS-Quelle, komplexe Wiederholungs-/Zeitzonenfälle und Linux-Abnahme bleiben
  offen. Portable Datei-/URI-Anhänge werden im Editor gespeichert und beim Ändern
  für unveränderte URIs mit ihren bestehenden Parametern bewahrt; native EDS-/Camel-
  Anhangsübertragung und Download-/Upload-Abnahme bleiben Linux-only. Der Editor erhält
  beim Ändern bestehende Teilnehmerparameter und
  unbekannte Alarmtypen, während einfache Display-Alarme separat gepflegt
  werden.
- [~] VTODO/VJOURNAL-Erstellung, VTODO-Status, Notizbearbeitung/-löschung,
  VTODO-Bearbeitung, Prioritäten, Fälligkeiten und Notizlöschung werden über
  den EDS-Adapter persistiert. ECal-Signalaktualisierung ist angebunden;
  die Aufgabenansicht bietet Suche sowie die Kategorien Alle/Heute/Wichtig/
  Überfällig/In Bearbeitung/Abgebrochen/Erledigt und filtert nach
  Statusänderungen erneut; die Notizensuche umfasst
  die Aufgaben-Seitenleiste zeigt die verfügbaren EDS-/lokalen Listen direkt
  als synchronisierte Navigation neben dem Quellenfilter;
  Titel, Inhalt, Zeitstempel und die erhaltene VJOURNAL-Komponente. VTODO-
  Kategorien können bei VJOURNAL-Notizen ebenfalls portabel angelegt,
  angezeigt und beim Bearbeiten als RFC5545-CATEGORIES erhalten werden.
  Statuswerte NEEDS-ACTION, IN-PROCESS, COMPLETED und CANCELLED sowie
  PERCENT-COMPLETE werden im portablen Task-Editor bearbeitet und gespeichert.
  Wiederkehrende Aufgaben können als RFC5545-RRULE eingegeben, geladen und
  geändert werden. Kalender und Aufgaben verwenden dafür denselben portablen
  RRULE-Rechenkern; VTODO-RDATE/EXDATE werden zusätzlich bearbeitet und die
  Aufgabenliste berechnet COUNT/UNTIL/EXDATE-bewusst die nächste Fälligkeit und
  verwendet sie für Heute-Filter, Sortierung und Anzeige.
  Die Aufgabenliste kann zusätzlich dynamisch nach RFC5545-CATEGORIES filtern;
  konfigurierte sowie in den geladenen VTODOs vorhandene Kategorien werden
  gemeinsam angeboten.
  Die Notizliste bietet denselben dynamischen Filter für VJOURNAL-CATEGORIES;
  konfigurierte sowie im geladenen Notizbestand vorhandene Kategorien werden
  gemeinsam angeboten.
  Die native EDS-Serienmutation und die Linux-Laufzeitabnahme bleiben offen.
  Kategorien werden im Editor geladen, als RFC5545-CATEGORIES gespeichert und
  beim Bearbeiten aktualisiert; einfache VTODO-Display-Erinnerungen werden als
  VALARM/TRIGGER gespeichert und beim Bearbeiten angepasst. Der portable
  VTODO-Editor bietet zusätzlich eine mehrzeilige Datei-/URI-Anhangsliste mit
  Dateiauswahl; bestehende ATTACH-Parameter unveränderter URIs bleiben beim
  Bearbeiten erhalten. Native EDS-Anhangsübertragung und Linux-Abnahme bleiben
  offen. Der portable Reminder-Service wertet diese Alarme aus, persistiert
  Zustellung/Snooze und stellt 10-Minuten-/1-Stunden-Snooze sowie Erledigt-
  Aktionen bereit. Komplexe Alarmtypen, native EDS-Aufgabenlisten-
  Synchronisation und weitere Evolution-Aufgabenlistenfunktionen fehlen noch.
  Termin-, Aufgaben- und Notizeditoren verwenden für ihre Quellenauswahl
  ebenfalls die cachefähige EDS-Entdeckung und zeigen bei EDS-Ausfall den
  geheimnisfreien importierten Quellenstand als Offline-Hinweis.
  Aufgaben können portabel nach Fälligkeit, Titel oder Priorität sortiert
  werden; Sortierfeld und auf-/absteigende Richtung werden über GSettings
  gespeichert, beim Start wiederhergestellt und bei Änderungen in den
  geöffneten Ansichten live synchronisiert. Die Memo-Liste bietet dieselbe
  persistente Auswahl für Änderungszeitpunkt, Titel und Quelle und übernimmt
  Änderungen ebenfalls live. Memo-
  Änderungen aktualisieren dabei auch die sichtbare sortierte Suchliste, und
  lokale VTODO-/VJOURNAL-Create/Modify/Remove-Operationen sind durch den
  app-eigenen ICS-Speicher persistent getestet. Termine, Aufgaben und Memos
  können jeweils als vollständige iCalendar-Komponente exportiert werden.

#### Phase 4 — Kontakte und Adressbücher

- [~] GTK4-rs-Kontaktansicht lädt Kontakte jetzt über die native EBook-/EDS-
  Brücke aus aktivierten Adressbüchern und fällt außerhalb von Linux explizit
  auf lokale vCards zurück, deren Quellordner über die Einstellungen wählbar
  ist; VCF-/vCard-Dateien können nicht überschreibend in den lokalen Bestand
  importiert werden. Der app-eigene vCard-Bestand unterstützt Erstellen,
  Bearbeiten und Löschen; explizit ausgewählte externe Ordner bleiben
  schreibgeschützt. Auswahl, Detail und Suche sind vorhanden.
  Die verfügbaren EDS-/lokalen Adressbücher werden zusätzlich als direkte
  linke Navigation neben dem bestehenden Quellenfilter angezeigt und bleiben
  mit ihm synchron.
  Die portable Dublettenerkennung kann Kontakte derselben schreibbaren Quelle
  nach normalisierten E-Mail-/Telefon-/Namensschlüsseln in einem
  Bestätigungsdialog zusammenführen. Dabei bleibt die Primär-UID erhalten,
  unbekannte vCard-/X-Eigenschaften werden übernommen und der lokale Bestand
  wird über einen atomaren Mehrkarten-Schreibvorgang aktualisiert; EDS-/Graph-
  Mutationen verwenden bis zur Linux-Abnahme den expliziten Modify-/Remove-
  Fehlerpfad ohne vorgetäuschte Transaktionsgarantie.
  Aus dem Kontakt-Detail öffnet die Mailaktion den Composer mit den
  ausgewählten E-Mail-Adressen vorbefüllt; die sichtbaren Adressen können
  außerdem direkt in die Zwischenablage kopiert werden.
  EBookClientView-Signale werden
  UID-basiert in die Kontaktliste eingepflegt; der Linux-Laufzeittest und die
  vollständige native vCard-Feldparität fehlen noch. Der portable Pfad kann
  strukturierte Namensbestandteile, mehrere zusätzliche RFC-Felder und
  Inline-Fotos darstellen bzw. editieren; URI-Fotos werden aus
  Datenschutzgründen nicht automatisch geladen. Inline-Fotos werden auch im
  Detailview nach Auswahl gesetzt; das Detailview zeigt zusätzlich Adressen,
  Kategorien, URLs, Geburtstage, Notizen und die übrigen sicheren RFC-Felder
  an. Die portable Kontaktbearbeitung zeigt außerdem zentrale
  `X-EVOLUTION-*`-Felder (Sortiername, Vorgesetzter, Assistenz, Partner,
  Blog/Video und Mailinglisten-/HTML-Präferenzen). Nachträgliche
  Dublettenänderungen invalidieren den GTK-Filter korrekt.
- [~] Der vCard-Parser/Serializer erhält mehrere E-Mail-Adressen,
  Telefonnummern, Adressen, Kategorien, URLs, Spitznamen und Geburtstage
  zusätzlich zu den bisherigen Kernfeldern sowie `N`, Jahrestag, Rolle,
  Geschlecht, Zeitzone, Geoposition, IMPP, RELATED und PHOTO; Roundtrip-,
  Inline-Bild- und Extended-Field-Tests sind vorhanden. vCard-Export ist
  verfügbar und beim Bearbeiten bestehender Kontakte bleiben nicht sichtbare
  Eigenschaften wie weitere Fotos, Kategorien, zusätzliche Kontaktwege und
  beliebige `X-*`-/`X-EVOLUTION-*`-Felder erhalten; die zentralen Evolution-
  Felder können portabel bearbeitet werden. Escaped Kommas in Kategorien
  überstehen Parser-/Serializer-Roundtrips.
- [~] EBook-/EDS-Quellen werden dynamisch erkannt; der native Adressbuch-
  Stream liefert vCards mit Quellen- und Kontakt-UIDs; der native Adapter
  unterstützt bereits Create/Modify/Remove mit Konfliktstatus, der app-eigene
  lokale Bestand ebenfalls. Vollständige EDS-GAL/LDAP-Felder und
  Konfliktauflösung fehlen noch; die portable Oberfläche kann mehrere
  Kontaktwege, strukturierte Namens-/Zusatzfelder, Kategorien und
  Inline-Fotos anzeigen und bearbeiten. Die Standardadressbuch-UID wird aus
  der Quellenauswahl persistent gespeichert und beim nächsten Start
  vorselektiert. Ein app-eigener `vcards`-Ordner bleibt nach dem Speichern
  seines Pfads weiterhin schreibbar; ausdrücklich externe vCard-Pfade bleiben
  dagegen read-only.
  Die Kontaktliste bietet zusätzlich einen dynamischen RFC6350-CATEGORIES-
  Filter; Kategorien aus dem geladenen EDS-/vCard-Bestand werden gemeinsam
  angeboten und bei Änderungen automatisch aktualisiert.
  Änderungen an der gespeicherten Standardadressbuchquelle werden bei
  geöffneter Kontaktansicht ebenfalls live in Auswahl und Filter übernommen.
  Auch der Kontakteditor verwendet dieselbe cachefähige Quellenentdeckung,
  bewahrt eine gespeicherte, aktuell nicht erreichbare Quelle sichtbar und
  bietet das lokale Adressbuch nur bei schreibbarer Verfügbarkeit an.
- [~] Lokale vCard-Kontakte werden asynchron als Autocomplete für To/Cc/Bcc im
  Composer angeboten und in den aktuellen Empfänger-Token eingesetzt; die
  Kontakt-Schnellsuche durchsucht Name, E-Mail, Organisation, Titel, Telefon,
  Notiz, Kategorien, Adressen und die erhaltene vCard. Der portable
  Adressbuchfilter kann EDS-Quellen bzw. das lokale Adressbuch auswählen und
  Dubletten anhand normalisierter E-Mail-/Telefonwerte anzeigen. Die
  Adressbuchauswahl ist als persistente Standardquelle in den Einstellungen
  verfügbar. Die portable Exchange-GAL-Suche ist über einen eigenen
  asynchronen Verzeichnis-Button an To/Cc/Bcc angebunden, verwendet weiterhin
  die lokale vCard-Autovervollständigung getrennt davon, setzt Treffer in den
  aktuellen Empfänger-Token ein und verarbeitet Graph-Pagination, Timeouts,
  fehlende Berechtigungen und Offlinefehler ohne den GTK-Thread zu blockieren.
  Native EBook-GAL/LDAP-Felder, echte EDS-/GOA-Credentials und die Linux-
  Abnahme gegen reale Verzeichnisdienste bleiben offen.

#### Phase 5 — Sicherheit und Datenschutz

- [~] WebKit-Sicherheitsgrundlagen und ein portabler HTML-Sanitizer sind
  vorhanden: JavaScript, aktive HTML-Elemente, Eventhandler, gefährliche
  URI-Schemata und CSS-Remote-Ressourcen werden vor dem Viewer entfernt;
  bekannte MIME-CID-Rasterbilder werden ausschließlich aus dem bereits
  geladenen Nachrichtenspeicher und mit Größenlimit eingebettet; WebKit
  blockiert externe Bilder standardmäßig.
- [~] OpenPGP über GnuPG/MIME: Der Composer bindet die Signatur- und
  Verschlüsselungsschalter jetzt tatsächlich in den Versand ein; eine
  portable GnuPG-Schicht erzeugt `multipart/signed` und
  `multipart/encrypted`, verwendet keine Shell-Aufrufe und meldet fehlende
  Schlüssel bzw. GnuPG-Fehler sichtbar. Fingerprint-/UID-Schlüssel werden
  aus dem maschinenlesbaren GnuPG-Schlüsselbund geladen und können als
  Standardschlüssel gewählt werden; lokale Entwürfe bewahren die Auswahl.
  Erkannte OpenPGP-RFC822-Daten bleiben für eine asynchrone GnuPG-Prüfung
  erhalten; `multipart/signed` wird tatsächlich verifiziert und
  `multipart/encrypted` bei verfügbarem Schlüssel entschlüsselt und im Viewer
  gerendert. Trust-/Widerrufsstatus, Passphrase-/Smartcard-Dialoge sowie die
  native Camel-/EDS-Anwendung und Linux-Laufzeitabnahme bleiben offen.
- [~] Portable S/MIME/CMS über OpenSSL ist als echter MIME-Pfad vorhanden:
  ausgehende Nachrichten können signiert und/oder verschlüsselt werden,
  eingehende `multipart/signed`- und `application/pkcs7-mime`-Nachrichten
  werden asynchron geprüft bzw. entschlüsselt. Zertifikat, privater Schlüssel,
  Empfängerzertifikate und optionaler CA-Bundle sind in den Einstellungen,
  Backups und lokalen Entwürfen verdrahtet; Fehler bei fehlenden Dateien oder
  Empfängerzertifikaten werden vor dem Versand gemeldet. Offen bleiben die
  native CamelSMIMEContext-Anwendung, Secret-Service/Keychain-Integration,
  Trust-/Widerrufs- und Ablaufdetails, PKCS#12/Token-/Passphrase-Flows sowie
  die Linux-Abnahme gegen echte Zertifikatsspeicher.
- [~] Remote-Content-Entscheidung pro Nachricht/Konto, Tracking-Schutz,
  sichere CID-Anhänge, Secret-Service und sichere Logs. Die portable
  Sanitizing-Strecke, sichere CID-Auflösung für bekannte Rasterbilder und per
  Nachricht gespeicherte Freigaben sind umgesetzt. Der Mail-Detailheader
  klassifiziert OpenPGP-/S/MIME-MIME-Hüllen und zeigt signierte oder
  verschlüsselte Nachrichten ausdrücklich als erkannt, aber ohne
  Verifikation/Entschlüsselung an; kontobezogene Freigabeverwaltung,
  Secret-Service, native Camel-/EDS-Kryptografie und Linux-/EDS-/Flatpak-
  Abnahme fehlen noch.

#### Phase 6 — Regeln, Suche und Enterprise

- [~] Filter-, Abwesenheits-, Suchordner- und Exchange-Dialoge existieren als
  UI/Service-Strecken; die nicht-geheime Abwesenheitskonfiguration (Status,
  Zeitraum sowie interne/externe Nachricht) wird portabel versioniert im
  app-eigenen Store gespeichert und in Backup/Restore aufgenommen. Der
  Microsoft-Graph-Pfad für GAL und OOF nutzt keine gespeicherten Geheimnisse,
  aktualisiert den Dialog asynchron und meldet Serverfehler getrennt vom
  lokalen Speichern. Die interaktive OAuth2-/EDS-/GOA-Anmeldung und die native
  Serverabnahme bleiben EDS/Camel- und Linux-spezifisch. Filterregeln werden
  versioniert persistent gespeichert,
  können bearbeitet, aktiviert, geordnet und entfernt werden und unterstützte
  lokale Eingangsaktionen werden ausgewertet. Der GTK4-rs-Filtereditor kann
  mehrere Bedingungen und Aktionen dynamisch hinzufügen, entfernen und als
  UND/ODER-Regel speichern. Importierte Regeln können mehrere Bedingungen
  (UND/ODER) und mehrere Aktionen verlustarm behalten. Jede gespeicherte Regel
  kann zusätzlich asynchron im lokalen Mailbestand manuell ausgeführt werden;
  der Ausführungsbericht zählt Treffer, Änderungen, Verschieben, Kopieren und
  Löschen und der lokale Status wird persistent im Sidecar gespeichert.
  Die portable Regelengine unterstützt Verschieben, Löschen und Kopieren über
  den app-eigenen Sidecar sowie Gelesen/Ungelesen und Flag setzen/entfernen als
  portable Aktionen; Stern-/Markierungsstatus und Wichtigkeitsstatus sind dabei
  getrennt und können in lokalen Regeln unabhängig gesetzt oder entfernt werden;
  Bedingungen können zusätzlich Ungelesen-, Stern-/Markierungs-, Wichtigkeits-,
  Anhangs-, Datums-, Ordner- und Junk-/Spamstatus auswerten; Datum wird dabei
  providerunabhängig aus RFC-822-/RFC-3339- und ISO-Werten normalisiert und
  unterstützt „vor“/„nach“;
  „weitere Filterregeln nicht ausführen“ beendet die lokale Regelkette an der
  importierten Reihenfolge. Spam-/Kein-Spam-Aktionen
  verwenden portabel die Junk-/Inbox-Platzierung. Lokale Kopien werden als
  eigene logische Datensätze mit unabhängigen Zuständen dargestellt. App-eigene
  Labels können portabel gesetzt, entfernt, angezeigt und über
  `label:` gesucht werden. Die GTK4-rs-Mailansicht bietet dafür eine reversible
  Einzel- und Batch-Aktion mit persistentem Sidecar und Undo/Redo; Graph-Kopien
  laufen serverseitig über den Graph-Copy-Endpunkt und sind im selben
  Command-/Undo-Pfad enthalten. Native EDS/Camel-Kopien und serverseitige
  Mehrfachlabels benötigen weiter den Camel-/Multi-Label-Pfad. Die lokale
  Regelbedingung unterstützt Betreff, Absender,
  Empfänger, Nachrichtentext, RFC822-Header, Ordner, Datum und Junk-/Spamstatus
  sowie die Vergleiche enthält, enthält nicht, ist, ist nicht, beginnt mit,
  endet mit, vor, nach und existiert. Alte
  app-eigene Filterdateien werden mit dem bisherigen „enthält“-Standard
  weitergelesen; Evolution-XML-Importe übernehmen bekannte Vergleichsnamen
  und melden unbekannte Operatoren transparent als verlustarm auf „enthält“
  zurück. Neue und bearbeitete Regeln werden vor dem Persistieren auf Namen,
  Bedingungen und erforderliche Aktionsziele geprüft; leere Editorzeilen
  werden nicht mehr stillschweigend verworfen.
- [ ] Camel-Filterdriver mit persistenten Regeln, Reihenfolge, Bedingungen,
  vollständigen Aktionen, Testlauf und Fehlerprotokoll.
- [~] Die lokale Suchsprache unterstützt Feld-, Label-, Status-, Ordner-, Datums-,
  Zitat- und Anhangsoperatoren (`in:`/`folder:`, `on:`, `older:`/`newer:`) sowie
  negative Evolution-Prädikate (`-from:`, `-body:`, `is:not-*`, `-has:`);
  Datumswerte werden zusätzlich als ISO-Kompakt-, RFC-3339-/RFC-822- und
  relative Werte (`today`/`yesterday`/`7d`) erkannt;
  portable Suchordner können gespeichert, bearbeitet,
  beim Start geladen und aus der Sidebar entfernt werden; ihre app-eigene
  JSON-Persistenz validiert neue Definitionen und ist für Save/Update/Delete
  separat testbar. Eine globale GTK4-rs-Suche
  durchsucht zusätzlich den lokalen Mail-, Kalender-, Aufgaben-, Notizen- und
  Kontaktbestand über einen gemeinsamen Service und kann den Suchbereich auf
  Alle, Mail, Kalender/Aufgaben/Notizen oder Kontakte einschränken. Der lokale
  Bestandsscan läuft debounced und abbrechbar in einem Worker, sodass die Suche
  den GTK-Hauptloop nicht blockiert; beim Schließen des Suchfensters werden
  Debounce-Quelle und laufender Scan ebenfalls abgebrochen. Der
  lokale Mail-Suchindex und die portablen Filterregeln verwenden für
  `body`/Nachrichtentext denselben bereinigten Klartext wie die Vorschau;
  Suchbegriffe werden dadurch auch über HTML-Tags und Zeilenumbrüche hinweg
  am sichtbaren Nachrichteninhalt gefunden. Der
  portable Graph-Mailpfad übersetzt kompatible positive Suchbestandteile in
  Outlook-KQL und integriert die paginierten Treffer in dieselbe Mailansicht;
  negative, status-, label-, ordner- und datumsabhängige Teile bleiben lokale
  Nachfilter. Native Server-/Camel-Suche, CamelVee-Suchordner und
  serverseitige Live-Aktualisierung gespeicherter Suchen bleiben offen. Lokale
  Mailtreffer berücksichtigen dabei auch Empfänger, Ordner, Labels,
  Ungelesen-, Stern-, Wichtigkeits-, Junk- und Anhangsstatus, Negationsprädikate,
  Dateinamen, Nachrichtentext und vollständige Kalender-/Aufgaben-/Notizdaten;
  aktivierbare Treffer wechseln in den passenden GTK4-rs-Hauptbereich. Die
  globale lokale Mail-Suche wertet dieselben Feldpräfixe für Absender,
  Empfänger, Betreff, Ordner, Labels und Dateinamen aus und unterstützt die
  Datumsprädikate `on:`, `after:`/`newer:` und `before:`/`older:` einschließlich
  negativer Formen für Mail, Kalender, Aufgaben und Notizen; die serverseitige
  Camel-Syntax bleibt davon getrennt. Der beim Öffnen gewählte Klartext-/Vorschau-
  Fallback bleibt außerdem bei einem anschließenden Neuladen durch HTML-/Theme-
  oder Schrifteinstellungen erhalten, damit eine Nachricht nicht wieder als
  leere Fläche endet.
- [~] Exchange/Microsoft 365: Der portable Graph-Pfad deckt Mail, Ordner,
  MIME-Versand/Entwürfe, Zustände/Labels/Kopieren, Kalender-CRUD mit grundlegenden
  Serien, Kontakte-CRUD, GAL und OOF ab; EWS-Kompatibilität bleibt nur soweit
  technisch notwendig. Interaktive OAuth2-/EDS-/GOA-Anmeldung, Delta-/Offline-
  Synchronisation, komplexe Serien-/Zeitzonen-/Meeting-Fälle und die reale
  Linux-Abnahme bleiben offen.

#### Phase 7 — Release- und Produktqualität

- [~] Nicht-geheime Evolution-.source-Dateien können aus einer ausdrücklich
  ausgewählten Profilkopie rekursiv gelesen und Mailkonten als app-eigene
  Profile übernommen werden; Passwörter, Tokens und private Schlüssel werden
  ignoriert. Nicht-geheime Identitäts-/Verbindungsdaten einschließlich
  Benutzername, Ports und Antwortadresse sowie unterstützte Evolution-Filter
  aus `filters.xml` und alle sicher
  lesbaren Text-/HTML-Signaturen aus `signature-list.xml` werden zusätzlich in
  versionierten portablen Beständen übernommen; die erste Signatur bleibt für
  den bestehenden Composer direkt ausgewählt. Mail-, Kalender- und
  Adressbuchquellen werden zusätzlich als geheimnisfreier Offline-Cache
  übernommen, damit importierte Quellen und ihre Aktivierung auch ohne
  erreichbares EDS nachvollziehbar bleiben. Valide lokale EML-/Maildir-/mbox-,
  iCalendar- und vCard-Dateien werden beim ausdrücklich gestarteten
  Profilimport zusätzlich nicht überschreibend in die app-eigenen Stores
  kopiert; lokale Maildir-/mbox-Ordnerpfade werden dabei erhalten und
  benutzerdefinierte Ordner registriert. Explizit exportierte nicht-geheime
  Evolution-Einstellungen aus INI-/GSettings-/DConf-Dateien werden jetzt
  typisiert übernommen; geheime Werte und alte lokale Quellpfade bleiben
  ausgeschlossen. Die native GSettings-Live-Synchronisation, vollständige
  native Camel-Ordnerstrukturen und die EDS-Registrierung fehlen noch. Nicht
  unterstützte Filterbedingungen/-aktionen werden beim Import jetzt als
  nachvollziehbare Warnung im Bericht und Log ausgewiesen, statt still
  verloren zu gehen; bewusst verworfene geheime `.source`-Felder werden nur
  gezählt und im Ergebnis transparent angezeigt, nie gespeichert oder geloggt.
- [~] App-eigene Einstellungen und JSON-Zustände können ohne Geheimnisse als
  portable Sicherung gespeichert und wiederhergestellt werden; die Sicherung
  umfasst auch beliebige app-eigene ICS-/vCard-Dateien unter den lokalen
  Importverzeichnissen sowie Reminder-/Signatur-Daten und akzeptiert ältere
  Einstellungen einschließlich des Benachrichtigungs-Schalters. Fehlende
  Felder älterer Sicherungsstände werden jetzt mit den tatsächlichen
  GSettings-Schema-Defaults ergänzt; dadurch bleiben auch Fenster-, Layout-,
  Mail-, Kalender-, Aufgaben-, Memo- und Composer-Einstellungen bei
  schrittweisen Upgrades lesbar. Der app-eigene Vorlagen- und
  Textbausteinspeicher ist ebenfalls enthalten. Neue Sicherungen enthalten
  zusätzlich je Datei eine SHA-256-Prüfsumme; beschädigte oder unvollständige
  Sicherungen werden vor dem Schreiben abgewiesen, ältere Sicherungen ohne
  Prüfsumme bleiben lesbar. Die Wiederherstellung validiert die Sicherung
  außerdem bereits vor dem Bestätigungsdialog und zeigt Dateianzahl sowie
  Integritätsstatus an. EML-, ICS-
  und vCard-Export sowie nicht überschreibende
  lokale EML/mbox-, ICS/VCS- und VCF/vCard-Importe sind im GTK4-rs-Frontend
  verfügbar. Ein technisches, inhaltsfreies Diagnosepaket und eine
  bestätigungspflichtige Löschung der app-eigenen Dateien/Einstellungen sind
  vorhanden; vollständige Evolution-Profilmigration, Diagnose-/Restore-
  Fehlerberichte und Upgrade-/Rollback-Prüfung fehlen noch.
- [~] Tastaturbedienung besitzt echte globale GTK-Aktionen für Navigation,
  Suche, Synchronisation, Sidebar, Mailaktionen und Modulwechsel; das
  Shortcuts-Fenster zeigt dieselben Beschleuniger. Die klassische Mailansicht
  sowie Kalender, Aufgaben, Memos und Kontakte setzen nun für Nachrichten-,
  Termin-, Aufgaben- und Kontaktaktionen Accessible-Labels, einschließlich
  dynamischer Sortier- und Statusaktionen. Lokalisierung, IME, RTL, HiDPI,
  vollständige AT-SPI-Prüfung und ein GNOME-HIG-Review fehlen noch.
- [~] Performance-Ziele für große Ordner, Speicher-/Thread-Leaks,
  Startup-Konkurrenz und
  Crash-Recovery und Telemetrie-freie Fehlerdiagnose. Der Mail-Startpfad
  hält Worker-I/O und GTK-Modellaufbau getrennt und verarbeitet große
  Nachrichten-/Threadmodelle inkrementell; Linux-Abnahme mit echten EDS-
  Konten, Provider-Latenz, Leakprüfung und Crash-Recovery bleibt offen.
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
| Mail lesen | Vorschaufenster, klassische oder moderne Mail-Anordnung, Threading, Sortierung, Gruppierung, Zitierstil, Schriftarten, Listendichte, HTML/Text, externe Bilder, Tracking, Markieren als gelesen (inkl. persistentem Verhalten beim Auswählen) | `[~]` |
| Mail empfangen | Konten, Aktivierung, Abrufintervall, IMAP-Ordner, Offline-Modus, Synchronisationsumfang, Junk/Spam, Quoten, Empfangsbenachrichtigung | `[~]` |
| Mail senden | SMTP/Transport, Authentifizierung, TLS, Port, Absender, Antwortadresse, Bcc, DSN/Lesebestätigung, Signatur, Autosave, gesendeter Ordner | `[~]` |
| Composer | HTML/Text, Formatvorlage, Schrift, Rechtschreibung, Wörterbücher, Zitate, Inline-Anhänge, Vorlagen, Snippets, Entwürfe, Undo/Redo | `[~]` |
| Konten & Identitäten | mehrere Identitäten, Alias-Adressen, Signaturen, Standardkonto, Ordnerzuordnung, Passwort/Secret-Service, OAuth2 | `[~]` |
| Kalender | Standardkalender, Arbeitswoche, Arbeitstage/-zeiten, Zeitzonen, Wochenstart, Erinnerungen, Snooze, Kategorien, Farben, Wetter/Feiertage | `[~]` |
| Termine | Wiederholung, Teilnehmer, iTIP/iMIP, freie Zeit, Einladungsantworten, private Termine, Alarme, Konferenzanbieter, Anhänge | `[~]` |
| Aufgaben & Memos | Standardlisten, Priorität, Status, Fälligkeit, Erinnerungen, Sortierung, Kategorien, VTODO/VJOURNAL-Sync | `[~]` |
| Kontakte | Standardadressbuch, Autocomplete, GAL/LDAP, vCard-Felder, Kategorien, Fotos, Dubletten, Telefon-/Adressformate | `[~]` |
| Sicherheit | OpenPGP, S/MIME, Schlüssel/Zertifikate, Trust, Ablauf/Widerruf, Verschlüsselungsstandard, sichere Speicherung, Remote-Content | `[~]` |
| Filter & Suche | Filterregeln, Reihenfolge, Bedingungen/Aktionen, gespeicherte Suchen, Suchordner, Volltext/Server-Suche | `[~]` |
| Oberfläche | Theme, Dark Mode, Dichte, Icons, Sidebar, View-Switcher, Shortcuts, Accessibility, Statusleisten | `[~]` |
| Migration | Evolution-Import, Export, Backup, Profilwechsel, Datenpfade, Upgrade-/Rollback-Verhalten | `[~]` |

Der portable Mail-Empfang unterstützt inzwischen neben Konto-/Ordnerauswahl,
Abrufintervall und Offline-Modus auch einen persistenten Seitenumfang von
25–1000 Nachrichten je Remote-Ordner. Der Wert wird für Kontoauswahl,
Ordnerauswahl, Graph-Suche und „Mehr laden“ gemeinsam verwendet und bei einer
Änderung live angewendet. Lokale Mailbestände werden weiterhin vollständig
angezeigt. Native IMAP/POP3-Empfang, Server-Junk/Spam, Quoten und
Desktop-Empfangsbenachrichtigungen bleiben bis zur Linux-Abnahme offen.

### Abnahmekriterium für den produktiven Ersatz

Das Projekt gilt erst dann als produktiver Evolution-Ersatz, wenn alle
Pflichtbereiche der Matrix implementiert sind, ein bestehendes Evolution-Profil
importiert werden kann, Mail/Kalender/Kontakte/Aufgaben synchron arbeiten und
ein kompletter Offline-/Online-/Fehler-/Wiederanlauf-Zyklus ohne Datenverlust
getestet ist. Bis dahin ist die Anwendung eine technische Vorschau und darf
nicht als vollständiger Ersatz beworben werden.

### Operative Entwicklungsreihenfolge

Der portable Kerncommit `10b0321` belegt 359/359 Workspace-Tests für den neuen
Mailoperations-Lifecycle, MIME-/HTML-Sicherheit, RFC822-Komposition,
Evolution-Suchsyntax, Recurrence-Grenzfälle, verlustarme vCard-Roundtrips und
geheimnisfreie Migration. Diese Belege schließen ausschließlich die atomaren
Service-Pakete; GTK-Worker-/Viewer-Integration und alle realen
EDS-/Camel-/ECal-/EBook-Abnahmen bleiben offen.

Die frühere, fortlaufend gewachsene Liste „Nächster Entwicklungsblock“ ist in
atomare, dauerhaft referenzierbare Aufgaben überführt. Die aktuelle Reihenfolge,
Abhängigkeiten, Plattformgrenzen, Abnahmekriterien und Evidenz stehen in
[ROADMAP_EXECUTION.md](ROADMAP_EXECUTION.md).

Die Reihenfolge beginnt bewusst mit täglich genutztem Mailbetrieb,
Main-Thread-Stabilität, MIME-Darstellung, Synchronisation, Composer und
Recovery. Erst danach folgen Groupware-, Enterprise-, Migrations-,
Einstellungs- und Release-Vollparität. Diese Priorisierung ändert nicht das
Abnahmekriterium: Produktivparität ist weiterhin erst erreicht, wenn alle
Pflichtbereiche dieser Datei und alle erforderlichen Linux-Abnahmen erfüllt
sind.
