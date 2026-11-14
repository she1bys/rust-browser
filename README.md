# Rust Browser

A fast, lightweight web browser for Windows. The core is Rust; pages are rendered by
Microsoft's WebView2 (the Chromium engine that ships with Windows 11), so real-world
sites work the way they do in Edge or Chrome.

## Download and install

1. Open the [latest release](https://github.com/she1bys/rust-browser/releases/latest) and download `RustBrowser-Setup-<version>.exe`.
2. Run it. No administrator rights are needed: it installs for your Windows account, adds a Start menu shortcut, and installs the Microsoft Edge WebView2 Runtime if your PC doesn't have it yet (Windows 10 and 11 usually do).
3. To make it your default browser, tick "Make Rust Browser my default browser" at the end of setup, or choose it later in Windows Settings > Apps > Default apps.

The app isn't code-signed yet, so Windows SmartScreen may warn about it: choose **More info > Run anyway**.
Prefer not to install? Download `rust-browser-<version>-portable.exe` and run it directly.
Uninstall from Settings > Apps; it asks whether to keep your bookmarks, history and settings.

## Features

- Chrome-style window: no separate title bar; the tab strip is the title bar, with minimize, maximize and close at its right end. Drag empty tab-strip space to move the window, double-click it to maximize
- Tabs: open, close, switch, smooth drag-to-reorder, middle-click to close, reopen closed tabs (`Ctrl+Shift+T`), favicons, links that ask for a new window open in a new tab (while sized popups like OAuth logins keep their opener)
- Split View: view two tabs side-by-side from the toolbar split button, the 3-dot menu, or the tab right-click menu ("Split with current tab"), with a toolbar swap button and automatic pane-focus tracking
- Memory saver (Settings > Performance: Off / Balanced / Maximum): tabs out of sight are first frozen (scripts pause, nothing is lost), then put to sleep (the page is unloaded and all its memory freed; it reloads when opened, shown faded in the tab strip). Tabs on screen, playing sound, downloading, or private are never put to sleep. Background tabs and a minimized window also ask the engine to use less memory. Measured with five websites open: 318 MB before, 194 MB after one minute, 165 MB once background tabs sleep
- Memory button (next to split view): shows the browser's total memory use, live. Click it for the memory manager: the total split into tabs and the browser itself, every tab with its memory and state (on screen, running, frozen, sleeping), a Sleep button per tab, the Memory saver switch, and "Free up memory" to put all background tabs to sleep at once (tabs playing sound and private tabs stay)
- Many tabs: tabs never get narrower than their icon plus close button; with more tabs than fit, the tab strip scrolls with the mouse wheel and fades at the edge that has more
- Tab hover card: rest the mouse on a tab to see its page's live memory use (updated every second, the same figure as Task Manager), and whether it is shared with other tabs, frozen or sleeping
- Smooth on high-refresh-rate screens: pages animate at the monitor's own rate (measured 165 frames per second on a 164 Hz monitor, using under 1% of one processor core). The engine's own frame pacing and power saving are left on, which is what keeps it smooth without heat or battery drain. The detected refresh rate is shown in Settings > About
- Address bar that accepts URLs, bare domains, local/UNC file paths, or a search, with autocomplete suggestions from bookmarks and history
- Bookmarks and History panels with live search (`Ctrl+F` focuses the search box in Bookmarks, History, and Downloads), plus a bookmark star button (`Ctrl+D`)
- Main menu (⋮, or `Alt+F`): new tab, new private tab, reopen closed tab, split view / swap panes, bookmarks, history, downloads, zoom in/out/reset, full screen, find on page, print, save page as, developer tools, settings, exit
- Tab right-click menu: new tab to the right, split with current tab / swap or close split panes, reload, duplicate, mute, close, close other tabs, close tabs to the right, reopen closed tab
- Private tabs (`Ctrl+Shift+N`): WebView2 InPrivate mode, marked in purple; they leave no history, saved tabs, cookies or download records
- Pop-up blocker: windows a page opens without a click are blocked, with an "Open" button to allow one anyway
- Sound indicator on tabs that play audio; click it to mute
- Security indicator in the address bar: lock for secure (https) pages, "Not secure" for plain http sites
- Crash recovery: a crashed page offers Reload; if the whole WebView2 engine stops, the browser restarts itself and reopens your tabs
- Hover link preview pill and per-tab zoom indicator with one-click reset (`Ctrl+0`)
- Full screen (`F11`) and HTML5 fullscreen video support
- Single-instance launch forwarding: opening `rust-browser.exe <url>` while already running opens the page in the existing window
- Settings panel:
  - Theme: follow Windows, light, or dark (websites that support it follow along)
  - Page zoom, and an optional Home button
  - On startup: open the home page, continue where you left off (lazy-loaded), or open a new tab
  - Home page address, and what new tabs show
  - Search engine: Google (default), Bing, DuckDuckGo, Brave Search or Ecosia
  - Where downloads are saved
  - Privacy: block pop-ups, tracking prevention (off / basic / balanced / strict), offer to save passwords, autofill forms, and protection from dangerous sites (Microsoft Defender SmartScreen, on by default)
  - Clear browsing data (cookies, cache, site data, saved passwords), clear history, and reset all settings
- Closing the last tab closes the browser, like Chrome (with a confirmation prompt if downloads are active)
- Smooth animations throughout (tabs opening and closing, panels, buttons, progress); turned off automatically if Windows is set to reduce motion
- Downloads tab (`Ctrl+J` or toolbar button): opens in its own tab (`about:downloads`) with live progress, cancel, open file, show in folder, open download folder, remove entries, and clear list; toolbar button animates with a progress ring and count badge when downloads are added or running. A download keeps running if you close its source tab
- Native Back, Forward (with disabled state), Reload / Stop
- Light and dark theme, following Windows
- Cookies, cache, site data, `settings.json`, `downloads.json`, `bookmarks.json`, `history.json` and `session.json` (open tabs) stored in `%LOCALAPPDATA%\RustBrowser`

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| Ctrl+T | New tab |
| Ctrl+Shift+N | New private tab |
| Ctrl+Shift+T | Reopen last closed tab |
| Alt+F or Alt+E | Open the main menu |
| F11 | Full screen |
| Ctrl+Shift+O | Open or close Bookmarks |
| Ctrl+Shift+B | Show or hide bookmarks bar |
| Ctrl+S | Save page as |
| Ctrl+W | Close tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl+1..8 / Ctrl+9 | Switch to tab 1–8 / last tab |
| Ctrl+L or Alt+D | Focus the address bar |
| F5 or Ctrl+R | Reload |
| Alt+Left / Alt+Right | Back / forward |
| Alt+Home | Home page |
| Ctrl+D | Bookmark / unbookmark current page |
| Ctrl+H | Open or close History |
| Ctrl+J | Open or close Downloads |
| Ctrl+, | Open or close Settings |
| Ctrl+0 | Reset page zoom to 100% |
| Esc | Close the active panel or modal |
| Ctrl+F, Ctrl+P, Ctrl+Plus/Minus | Find, print, zoom (built into WebView2) |
| F12 | Developer tools |

## Change it yourself

Rust Browser is open source under the [MIT license](LICENSE): you're free to download the code, change it, and share or publish your own version. Keep the license notice when you do.

### 1. Install the tools (once)

- Windows 10 or 11 with the [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) (preinstalled on Windows 11)
- [Git](https://git-scm.com), [Rust](https://rustup.rs) (stable, MSVC toolchain) and Visual Studio 2022 Build Tools with the "Desktop development with C++" workload. From a terminal:

```bash
winget install Git.Git Rustlang.Rustup
```

```bash
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

### 2. Get the code and run it

```bash
git clone https://github.com/she1bys/rust-browser.git
```

```bash
cd rust-browser
```

```bash
cargo run
```

The first build downloads and compiles the dependencies and takes a few minutes; later builds take seconds. Edit the files (see [How it is put together](#how-it-is-put-together)) and run `cargo run` again to see your change. The browser UI is plain HTML, CSS and JavaScript in `ui/`; the browser itself is Rust in `src/`.

### Build commands

Optimized build (output: `target\release\rust-browser.exe`):

```bash
cargo build --release
```

Run the tests:

```bash
cargo test
```

Open a specific page or search at startup:

```bash
cargo run -- rust-lang.org
```

Build the installer (needs [Inno Setup 6](https://jrsoftware.org/isinfo.php); output in `target\installer`):

```bash
cargo build --release
iscc installer\rust-browser.iss
```

## Releases

Pushing a version tag that matches `version` in `Cargo.toml` makes GitHub Actions build, test and publish a release with the installer and the portable .exe ([release.yml](.github/workflows/release.yml)):

```bash
git tag v0.1.0
git push origin v0.1.0
```

Every push and pull request is checked by [ci.yml](.github/workflows/ci.yml) (formatting, Clippy, tests, release build).

## Contributing

Bug reports, ideas and improvements are welcome: open an [issue](https://github.com/she1bys/rust-browser/issues) or a pull request. See [CONTRIBUTING.md](CONTRIBUTING.md).

## How it is put together

```
src/main.rs            window + event loop
src/browser.rs         tab manager: creates, shows, hides and closes web views
src/ipc.rs             messages from the browser UI to Rust
src/settings.rs        saved settings (theme, startup, search engine, zoom, ...)
src/session.rs         the open tabs, saved for "continue where you left off"
src/bookmarks.rs       saved bookmarks and search
src/history.rs         browsing history and search
src/downloads.rs       the downloads list and its saved file
src/download_hook.rs   WebView2 download events: progress, completion, cancel
src/display.rs         monitor refresh-rate detection and the WebView2 engine's start-up options
src/memory.rs          per-process memory (Task Manager's figure) and which processes draw a page
src/shortcut_hook.rs   browser shortcuts pressed while a page has focus
src/webview_hooks.rs   WebView2 hooks (history, favicon, status text, fullscreen, zoom,
                       sound, crashes, pop-ups) and actions (find, print, save, mute, privacy)
src/menu.rs            borderless floating popup menus: the main menu and tab right-click menu
src/single_instance.rs named-pipe forwarding to an already-running instance
src/url_input.rs       address bar text -> URL or search
ui/index.html          the browser UI: tab strip, toolbar, and panels
ui/menu.html           popup menu UI (icons, zoom bar, keyboard navigation)
```

The window holds one small web view for the UI at the top and one web view per tab below
it. Only the active tab is visible. While a panel is open, the UI web view
is stretched over the whole window.

The UI and Rust talk over a JSON message channel. Web pages are untrusted and have no
channel to the browser at all: page titles, loads, downloads, favicons, history state and
shortcut key presses all come from WebView2 itself.

## License

[MIT](LICENSE). Copyright (c) 2026 Rust Browser contributors.
