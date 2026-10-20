# Fossick’s search window

This describes the graphical frontends: what they show, how they behave, and how they fit
into the desktop. The Qt/Kirigami frontend (`fossick-qt`, Phase 3) implements everything here.
The GTK/libadwaita frontend (`fossick-gtk`, Phase 5) will follow the same design. The logic both
share lives in the toolkit-free crate `fossick-ui`: the daemon session, the row cache, columns,
filters, settings, history, desktop actions, the single-instance service, the global shortcut,
and the choice of frontend.

Behaviour and search syntax follow voidtools Everything. Names, icons and branding are
Fossick’s own.

## Starting it

| Command | Effect |
|---|---|
| `fossick --gui [QUERY]` | Opens the search window, or brings the running one forward. With `QUERY`, searches for it in a new tab. |
| `fossick --toggle` | Shows the window, or hides it if it is in front. Bind it to a key (see [Global shortcut](#global-shortcut)). |
| `fossick --settings` | Opens the settings window. |
| `fossick-qt [--search TEXT] [--new-window] [--toggle] [--settings [PAGE]]` | Starts the Qt frontend directly. `PAGE` is `general`, `search`, `index` or `shortcut`. |

`fossick --gui` picks the frontend by desktop: `fossick-qt` on KDE Plasma and LXQt,
`fossick-gtk` elsewhere (`XDG_CURRENT_DESKTOP`). If only one is installed, that one is used.
The choice can be fixed in Settings → General (“Interface for `fossick --gui`”). It exits with
status 6 if no frontend is installed.

**One process per session.** The first window owns the D-Bus name `io.github.oreo1298.Fossick`
on the session bus. Starting the GUI again, or running `fossick --gui`, `--toggle` or
`--settings`, forwards the request to it through the interface `io.github.oreo1298.Fossick1`
(methods `Toggle`, `Activate`, `NewWindow`, `Search(text)` and `Settings`; each takes the caller’s
activation token). The caller’s `XDG_ACTIVATION_TOKEN` (or `DESKTOP_STARTUP_ID`) is passed on,
so Wayland compositors let the window take focus.

## Layout

```
┌───────────────────────────────────────────────────────────────────────────┐
│ [tab 1] [tab 2] [×]                                       (only with tabs) │
│ 🔍 search field…                                   [Everything ▾]   [≡]   │
│ ℹ banner: user mode / errors                                               │
│ Name ▲            │ Path                  │ Size   │ Date Modified         │
│ 📄 **rep**ort.pdf │ /home/ana/Documents   │ 120 KB │ 20/10/2026 09:14      │
│ …                                                                          │
│ 41 objects, 2 selected   0.4 ms                     Indexing /home…        │
└───────────────────────────────────────────────────────────────────────────┘
```

- **Search field**, focused on start. Every keystroke runs a search; an older search still
  running is dropped. Results arrive as soon as the daemon answers, and the visible rows are
  fetched in pages of 200 as you scroll. The list is virtualized, so millions of results cost
  nothing extra in the window.
- **Filter list**: Everything, Audio, Compressed, Document, Executable, Folder, Picture, Video,
  plus your own (Settings → Search). A filter is a search that results must also match
  (`audio:` …).
- **Menu button** (F10): match options, recent searches, bookmarks, new tab or window,
  settings, quit.
- **Banners**: an information banner when the system index service isn’t running and your own
  (user-mode) service searches your home folder only; an error banner when the service can’t be
  reached or a search is invalid.
- **Results table**: the name with its file-type icon and the matched part in bold, then the
  path, size and date modified. The optional columns are Date Created, Date Accessed, Extension,
  Type, Permissions, Owner, Run Count and Volume. Click a header to sort (again to reverse).
  Drag headers to reorder and drag their edges to resize. Right-click a header to show or hide
  columns. Order, widths, visibility and sort are saved. Results on unplugged drives are greyed
  out. Results update live while the window is open.
- **Status bar**: number of results and of selected ones, how long the search took in the
  daemon, and the indexing state (“Indexing /home…”, volumes that are only checked
  periodically, or the number of indexed volumes; hover for every volume’s state and watch
  method).
- **Tabs and windows**: each tab is its own search; each window has its own tabs. Tabs appear
  once there is more than one.

## Keyboard

| Key | Action |
|---|---|
| Ctrl+F, F3, Ctrl+L | Focus the search field and select its text |
| Down, Enter (in the search field) | Go to the results (Enter also remembers the search) |
| Alt+Down (in the search field) | Recent searches |
| Up (on the first result) | Back to the search field |
| Up/Down, Page Up/Down, Home/End | Move; with Shift, extend the selection; with Ctrl, move without selecting |
| Ctrl+Space | Add or remove the current result from the selection |
| Ctrl+A | Select all |
| Enter | Open (with the default application, through the OpenURI portal) |
| Ctrl+Enter | Show in the file manager with the item selected |
| Alt+Enter | Properties |
| Ctrl+C / Ctrl+Shift+C | Copy names / full paths |
| F2 | Rename |
| Delete | Move to the trash |
| Shift+Delete | Delete permanently (after confirmation) |
| Menu, Shift+F10 | Context menu of the current result |
| Esc | Clear the selection and go back to the search field |
| Ctrl+I / Ctrl+B / Ctrl+U / Ctrl+R | Match case / whole words / path / regular expressions |
| Ctrl+D | Bookmark the current search |
| Ctrl+T, Ctrl+W, Ctrl+Tab, Ctrl+Shift+Tab | New tab, close tab, next and previous tab |
| Ctrl+N | New window |
| Ctrl+, | Settings |
| F10 | Main menu |
| Ctrl+Q | Quit |

Mouse: click selects, Ctrl+click adds or removes, Shift+click selects a range, Ctrl+Shift+click
adds a range. Double-click opens. Right-click opens the context menu. Dragging results to
another application drops them as `text/uri-list` (copy or link).

## Actions on results

| Action | How |
|---|---|
| Open | OpenURI portal (`OpenFile` with a file descriptor), else `xdg-open`. Opening counts as a run (Run Count column, `runcount:` and `dr:`). |
| Open With… | The portal’s application chooser (needs xdg-desktop-portal). |
| Show in Folder | `org.freedesktop.FileManager1.ShowItems`, else `xdg-open` on the folder. |
| Copy Name / Copy Full Path | Clipboard, one per line. |
| Rename | Within the same folder. Names with `/`, `.` and `..` are refused, and so is a name that already exists (`renameat2` with `RENAME_NOREPLACE`). |
| Move to Trash | The freedesktop.org trash specification, including trash directories on other drives. |
| Delete Permanently | After a confirmation naming the number of items. Folders are deleted with their contents; symlinks are removed, not followed. |
| Properties | Location, type, link target, size, the four dates, permissions, owner, inode and link count, read from the filesystem. |

Failures appear as a passive notification at the bottom of the window.

## Recent searches and bookmarks

- **Recent searches** are remembered when you press Enter in the search field or open, show or
  copy a result. The newest 100 are kept in `$XDG_STATE_HOME/fossick/history` (default
  `~/.local/state/fossick/history`, mode 0600). Alt+Down in the search field and Menu → Recent
  Searches list them. Settings → Search turns this off and clears the list.
- **Bookmarks** (Ctrl+D, Menu → Bookmarks) save the search text, filter, match options and sort
  order under a name. Choosing one restores all of them. Rename or remove bookmarks in
  Settings → Search.

## Settings

Settings take effect immediately and are shared by all windows. They are stored in
`$XDG_CONFIG_HOME/fossick/gui.toml` (default `~/.config/fossick/gui.toml`). Hand-edited files are
repaired on load: missing columns are added, duplicates dropped, and unnamed filters or bookmarks
removed.

| Page | Settings |
|---|---|
| General | Colours (follow the system, light, dark); the interface `fossick --gui` opens; tray icon and close-to-tray; hidden files; columns; version and licence |
| Search | Default match options; your own filters (name + search); bookmarks; recent searches (on/off, clear) |
| Index | The index service’s own settings (below) |
| Shortcut | The global shortcut (below) |

**Index settings** belong to the index service (`fossickd`), not to the window: only these
volumes, excluded folders, excluded name patterns (`node_modules`, `*.o`), network folders to
poll, tmpfs, snapshots, access times, keeping unplugged drives searchable, and the watch
method. They are edited in the service’s configuration file. All other keys in the file are
kept, but its comments are not. **Apply** saves them and makes the service reload. Volumes
whose exclusions changed are rescanned; volumes no longer wanted are emptied.

- For the **system service** (`/etc/fossick/fossickd.toml`), Apply runs
  `pkexec fossick --install-config FILE`, which asks for an administrator password. That command
  installs the file (mode 0644), reloads the service, and puts the old file back if the service
  rejects the new one. **Rescan Everything** runs `pkexec fossick --rescan`. The polkit action
  `io.github.oreo1298.Fossick.admin` (`packaging/polkit/`) covers both and keeps the
  authorization for a few minutes (`auth_admin_keep`). Nothing else in the window needs
  elevated rights.
- For your own **user service** (`~/.config/fossick/fossickd.toml`), Apply writes the file and
  asks the service to reload. The old file comes back if the service rejects it.

The window edits the service the search windows use: the system one when it runs, otherwise
yours (the user-mode banner is showing then).

## Global shortcut

Settings → Shortcut → “Show or hide the search window from any application” registers a
shortcut through the **GlobalShortcuts desktop portal** (KDE Plasma 6, GNOME 48 and later,
Hyprland, and others whose portal backend implements it). The keys entered there (default
`CTRL+ALT+space`, in the [shortcuts specification](https://specifications.freedesktop.org/shortcuts-spec/latest/)
format) are only a suggestion. The desktop asks you to confirm, may let you pick other keys, and
keeps the choice. The state line shows the keys the desktop assigned. “Change Keys…” opens the
desktop’s own shortcut settings (portal version 2). Fossick names itself to the portal with the
host app registry (`org.freedesktop.host.portal.Registry`) where the portal has one. Older
portals identify the app by its systemd scope instead.

The shortcut is off by default, because enabling it makes the desktop show a dialog.

**Without the portal** (X11 sessions, older desktops, most window managers), bind a key to the
command `fossick --toggle` in the desktop’s keyboard settings:

- **KDE Plasma:** System Settings → Keyboard → Shortcuts → Add New → Command or Script →
  `fossick --toggle`, then assign a key.
- **GNOME:** Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts → `+`, with
  the command `fossick --toggle`.
- **Sway / i3:** `bindsym $mod+Shift+f exec fossick --toggle`.
- **Hyprland:** `bind = SUPER SHIFT, F, exec, fossick --toggle`.

`--toggle` starts the GUI if none runs. Otherwise it hides the window when that window is in
front and brings it forward otherwise, with the activation token, so Wayland compositors allow
the focus change.

## Tray

Settings → General → “Show an icon in the system tray” adds a StatusNotifierItem (KDE Plasma,
and GNOME with the AppIndicator extension; Qt falls back to the XEmbed tray on X11). Clicking the
icon shows or hides the window. Its menu has Show or Hide, New Window and Quit. With
“Closing the window keeps Fossick in the tray”, closing the last window only hides it.

## Desktop integration

- Desktop entry `packaging/desktop/io.github.oreo1298.Fossick.desktop` (`Exec=fossick --gui`, with
  a Settings action) and a scalable icon `io.github.oreo1298.Fossick.svg`. The icon is also built
  into the program for the window and tray when it isn’t installed.
- The Wayland app id is the desktop file name (`QGuiApplication::setDesktopFileName`), so docks
  and task switchers match the window to the desktop entry.
- Icons follow the desktop’s icon theme. Icons the theme lacks come from Breeze, which the
  packages depend on.

## Appearance and platforms

- **Style:** the Qt frontend uses the desktop’s Qt Quick style: `org.kde.desktop` on Plasma
  and LXQt (system colours, accent, fonts and icons), and Fusion with the system palette
  elsewhere. Settings → General → Colours overrides light or dark (Qt 6.8’s
  `QStyleHints::setColorScheme`).
- **Wayland first, X11 supported**: nothing uses X11-only APIs. Window activation uses the
  xdg-activation token, and the global shortcut uses the portal.
- **HiDPI:** Qt’s per-monitor scaling with fractional factors. Sizes are in Kirigami grid units
  and icons are SVG.
- **Reduced motion:** the window has no animations of its own. Kirigami’s follow the platform
  setting.
- **High contrast:** every colour comes from the Kirigami theme (the system palette). There
  are no hard-coded colours.

## Accessibility

- Every control has an accessible name. The table exposes the Table role. Each cell exposes the
  Cell role, its plain text (no highlight markup) and whether it is selected.
- Focus stays on the table while the current row moves, so the row is announced (“report.pdf,
  selected, row 3 of 41”) through `Accessible.announce` (Qt 6.8).
- Everything works from the keyboard: see [Keyboard](#keyboard). The main and context menus
  open with F10 and Menu/Shift+F10.
- Not tested yet: a screen reader (Orca) on a real Plasma or GNOME session. The offscreen
  test builds can’t exercise one.

## Translations

All user-visible QML strings go through `qsTr`, and counts use Qt’s plural forms (`%n`).

- `crates/gui-qt/translations/fossick.ts` is the template, and
  `crates/gui-qt/translations/update.sh` refreshes it from the sources (Qt’s `lupdate`).
- To translate, copy the template to `fossick_<lang>.ts` (for example `fossick_de.ts`),
  translate it with Qt Linguist, and rebuild. `build.rs` compiles every `fossick_*.ts` with
  `lrelease` into the program. The language follows the system locale.
- `fossick_en.ts` holds only the English plural forms (“1 object”, “2 objects”).
- Not translated yet: messages that come from Rust code (daemon errors, action failures).

## Files

| File | Contents |
|---|---|
| `~/.config/fossick/gui.toml` | Window settings, filters, bookmarks, columns |
| `~/.local/state/fossick/history` | Recent searches (0600) |
| `~/.config/fossick/fossickd.toml` | Your user index service’s settings |
| `/etc/fossick/fossickd.toml` | The system index service’s settings (root) |

## Testing

`tests/gui/screenshot.sh OUT_DIR [BIN_DIR]` starts a user-mode index service over a small fixture
tree and saves offscreen screenshots of:

- the empty search;
- a search;
- every settings page;
- a second launch handing its search to the running window (`fossick --gui holiday`).

CI runs it in Ubuntu 26.04 with the KDE style and with Fusion, and uploads the screenshots as an
artifact. The toolkit-free parts have unit and integration tests in `crates/ui/tests`:

- the session against a scripted daemon;
- single instance on a private D-Bus daemon;
- the global shortcut against a scripted portal;
- applying index settings against a scripted daemon.

## Not done yet

- Preview pane (Phase 6), fuzzy matching and frecency ordering (Phase 6).
- Translations other than English.
- Testing with a real screen reader, a real portal and a real tray on Plasma and GNOME.
