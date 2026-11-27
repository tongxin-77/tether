# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). This file mirrors the Simplified Chinese [CHANGELOG.md](CHANGELOG.md) version by version.

## [Unreleased]

## [0.7.1] - 2026-11-27

### ✨ Added

- **Resizable sidebar**
  - Drag the gap between the sidebar and the terminal to change its width; the width is remembered
- **Drag to reorder sessions and groups**
  - Drag a session within its group, or drag a group header to reorder groups; the order is kept across restarts
  - Dragging outside the window still opens the session in a separate window
- **Workspace grouping and changes in the hub web page** (requires the matching new tether-hub)
  - The web page groups by machine › workspace › session, with the same changes panel as the desktop app
  - Respects the "Allow takeover" switch; commit details are not yet available for SSH sessions

### 🔧 Changed

- **Roomier sidebar and simpler group headers**
  - Wider default width, taller rows and more spacing between groups
  - Group headers keep only the project name, SSH host and the new-session button; branch and change count move to the header tooltip
- **Session memory moved to the hover tooltip**, marked "(high)" when it is high
- **The thinking status dot now uses an expanding halo animation**

### 🐛 Fixed

- **Status text at the end of sidebar rows is colored again**, and the background state has its own color

## [0.7.0] - 2026-11-26

### ✨ Added

- **Sessions grouped by workspace**
  - Local sessions are grouped by git repository, with sessions in subfolders showing their relative path; sessions outside a repository are grouped by folder; SSH sessions are grouped by host and remote repository
  - Group headers show project name, host, branch, a status summary and the change count; groups can be collapsed (kept across restarts), and `+` opens a new session in that workspace
  - `Ctrl+Shift+K` filters sessions by name, path or host
- **Changes panel** (`Ctrl+Shift+J`, or click the change stats in the card header)
  - "Uncommitted" lists changed files with added/removed lines and totals, plus a diff with old/new line numbers; untracked files are marked "new"
  - "Commits" lists recent commits, with the files and diff of each commit
  - Switch to other workspaces; `↑` `↓` select files, `Enter` previews, `Esc` closes; the panel height is resizable and remembered
  - Also works for SSH sessions (requires the new background service); commit details are not yet available for remote sessions
- **Always-visible change stats in the card header**: shows "N files +added −removed" and refreshes after each Claude turn
- **Session names use the Claude conversation title**
  - Taken from the `/rename` title, the auto-generated title, the summary, or the first message, in that order; sessions renamed in Tether keep their name
  - Hover to see the original name; the hub web page shows the same name
- **`Ctrl+A` selects the input box in Claude sessions**
  - `Ctrl+C` copies the selection; when Claude is idle, `Backspace` / `Delete` clears the input box
- **Editable address bar in the Files panel**
  - Click to type a path and press `Enter` to go there; supports `~`, relative paths and quoted paths; a file path opens its folder and selects the file
  - Selecting a folder shows its path in the address bar; right-click "Open folder" or press `Enter` to enter it
- **README in English, Traditional Chinese and Japanese**

### 🔧 Changed

- **Title bar, brand and session info merged into a single top bar; compact single-line sessions in the sidebar**
  - The session name above the terminal is gone, giving the terminal more room
  - The close button appears on hover, memory and takeover become small tags at the end of the row, and unread session names are bold
  - Host info moved below the Files panel
- **An update dialog opens at startup when a new version is available**, instead of only a badge in the sidebar
- **The UI is split into independent components**, preparing for adjustable panel layouts
- **`Esc` closes open menus and panels first**

### 🐛 Fixed

- **No longer stuck on "Thinking" after interrupting Claude with `Esc`**
- **Idle teammates no longer count toward "background"** (the similar fix in 0.5.9 never took effect for teammates)

## [0.6.0] - 2026-11-23

### ✨ Added

- **Multi-language interface: Simplified Chinese, English, Traditional Chinese and Japanese**
  - Follows the Windows display language by default; can be changed under Settings → Appearance → Language, taking effect immediately
  - System notifications, error messages and SSH / sudo prompts from the background service use the same language
  - The installer picks its language from the system; update notes are shown in Chinese or English

## [0.5.9] - 2026-11-23

### ✨ Added

- **Files can be edited and saved in the preview panel**
  - YAML, `.env`, `.gitignore`, `env.example`, JSON, TOML, INI, `Dockerfile`, scripts, txt and more open straight in the editor on double-click
  - Markdown shows the preview by default; click "Edit" to change the source, and `Ctrl+S` saves and returns to the preview
  - Right-click any file and choose "Open as text"; binary files are reported as not openable
  - The editor has line numbers, syntax highlighting and find / replace (`Ctrl+F` / `Ctrl+H`), with colors following the theme
  - YAML syntax errors are marked by line, and saving with errors asks for confirmation
  - Saving keeps the original encoding (GBK, UTF-8, BOM, UTF-16) and line endings; if the file was changed elsewhere you can overwrite / reload / cancel
  - With unsaved changes, closing the panel, switching sessions or opening another file asks for confirmation first
  - Remote SSH files can be edited too, written as root when the file panel is elevated; files over 5 MB are read-only
- **Preview supports txt / log files**

### 🔧 Changed

- **Preview fills the panel width, and large files scroll more smoothly**
  - The preview limit is raised to 20 MB; GBK-encoded text no longer shows garbled characters

### 🐛 Fixed

- **Switching sessions no longer stutters with GPU acceleration on**
  - The 8 most recently used sessions keep GPU rendering, so switching back and forth needs no rebuild
- **Idle Claude teammates no longer keep showing "Running in background"**

## [0.5.8] - 2026-11-19

### 🐛 Fixed

- **The terminal recovers after Claude exits abnormally**
  - When the Claude process ends unexpectedly (without a normal exit), its status in the sidebar is cleared within about 10 seconds
  - The terminal automatically leaves Claude's full-screen view and turns off mouse mode, so moving the mouse no longer types garbage into the shell
  - Terminals taken over from the hub web page are reset as well; the conversation can still be resumed
- **The terminal scrollbar is hidden while full-screen programs run**
  - Claude's full-screen UI, vim and similar programs have no scrollback, so the right-hand scrollbar is hidden; it returns once you are back at the shell

## [0.5.7] - 2026-11-19

### ✨ Added

- **The SSH file panel can be elevated to root**
  - The shield button in the file panel title bar toggles "Browse as root"; it applies per SSH connection and reverts when disconnected
  - Tries passwordless sudo, then the saved login password for the host, and only then asks for the sudo password
  - While elevated, the panel shows a root badge and warning, and deletes, uploads and other operations run as root
  - When the terminal prompt becomes root (e.g. after `sudo -i`), Tether offers to elevate the panel too instead of switching automatically
- **GPU-accelerated terminal rendering**
  - The current terminal is rendered with WebGL, making heavy Claude output and scrolling smoother
  - Can be turned off under Settings → Appearance; falls back to the previous renderer when the GPU is unavailable or fails
- **Restart the background service from Settings**
  - "About & Updates" shows the background service version and offers a "Restart background service" button

## [0.5.6] - 2026-11-19

### ✨ Added

- **New sessions can be created on this machine from the hub web page** (requires tether-hub 0.5.0)
  - Supports local sessions, SSH sessions and resuming Claude conversations
  - The web page opens the new session directly; it also appears in the desktop sidebar without switching to it
  - Requires allowing hub takeover on this machine; SSH sessions that need a password or fingerprint confirmation fail instead of prompting on the desktop
- **Adjustable font sizes**
  - The overall default font size is 2 points smaller; the UI and terminal font sizes can be adjusted in Settings
  - With the mouse over the terminal, `Ctrl` + mouse wheel changes the terminal font size directly

### 🔧 Changed

- **Pasting images into Claude with `Ctrl+V` is much faster**
  - When Claude runs in a local session, Tether saves the clipboard image to a temporary file and pastes its path, instead of waiting for Claude to read the clipboard
  - SSH sessions, or cases where saving fails, still use the previous method

## [0.5.5] - 2026-11-17

### ✨ Added

- **The session list shows memory usage**
  - Counts the shell and all its child processes (Claude, MCP servers, etc.), matching the "Memory" column in Task Manager
  - Refreshes every 5 seconds and turns to a warning color above 1 GB; not shown for remote SSH sessions
  - Takes effect after restarting the background service (click "Restart background service" in the banner after updating)
- **Update prompts show notes for every version not yet installed**
  - When updating across several versions, the changes of every version after the current one are listed
  - Update notes are rendered as Markdown (headings, bold, lists)

### 🐛 Fixed

- **Links in Claude output open with a single click**
  - Web links open in the default browser, local file links in the associated app
  - Previously the click was intercepted and nothing happened
- **Links and file paths can be selected by dragging and copied**
  - Previously, dragging over a link and releasing the mouse was treated as a click, so it could not be copied

## [0.5.4] - 2026-11-16

### ✨ Added

- **Send files to Claude in one click**
  - Right-click in the file tree and choose "Send to Claude" to type `@path` into the current session (without Enter); multiple selection is supported
  - Paths inside the session directory are relative, and quoted when they contain spaces
  - Dragging files from Explorer onto the terminal inserts their paths, or `@path` while Claude is running; dropping on the file tree still uploads
- **Resume Claude conversations**
  - The "New session" menu has "Resume Claude conversation…", listing past conversations in the directory (title, time, message count) with filtering
  - The selected conversation is resumed in a new session with its original permission mode
  - Conversations already open in a session are marked "Open"; clicking one switches to it

## [0.5.3] - 2026-11-16

### ✨ Added

- **Themes expanded to 6 styles, each with light and dark variants**
  - Styles: Cream 1 (formerly Cream / Latte Night), Minimal, Cream 2, Frosted Glass, Nordic Morandi, Modern Dark
  - Choose the style in Settings and toggle light / dark with the button at the bottom left of the sidebar
  - Previously selected Cream, Latte Night and Glass themes map to the new themes automatically
- **Click the branch in the card header to switch branches**
  - Lists local and remote branches; choosing a remote branch creates a local branch of the same name that tracks it
  - When uncommitted changes conflict, git's error is shown and nothing is stashed automatically
- **View commit history**
  - The latest 200 commits with the merge graph, branches / tags, author and time
  - Click a commit to see the changed files and diff, and copy its hash

### 🔧 Changed

- **Terminal font size follows the screen resolution**
  - 14 on 4K and larger screens, 13 elsewhere, for a more compact layout

### 🐛 Fixed

- **The Frosted Glass theme no longer shows a gray block in the terminal area that does not match the card edges**

## [0.5.2] - 2026-11-16

### 🔧 Changed

- **Background shells no longer count as "Running in background"**
  - Only subagents and teammates count as background tasks
  - After Claude Code starts long-running processes such as dev servers with a background shell / monitor, the turn shows "Done" and notifies when it ends

### 🐛 Fixed

- **Terminal content no longer gets squashed after a hub takeover ends**
  - When switching to a session that was taken over by a narrow screen such as a phone, or when the current session's takeover ends, the screen and scrollback are cleared and reloaded
  - Some scrollback lines written at the narrow width during the takeover may still appear narrow

## [0.5.1] - 2026-11-16

### ✨ Added

- **New "Glass" theme**
  - Gradient wallpaper background with translucent frosted sidebar, cards and file panel
  - Switch it in Settings or at the bottom of the sidebar; the wallpaper fades with the window transparency
- **Session card headers show the git branch**
  - Shows the current branch of the session's start directory, e.g. `⎇ main`; a detached HEAD shows the first 7 characters of the commit
  - Refreshes when switching sessions or when the window gains focus, and every 4 seconds
  - Local sessions only; hidden when the directory is not in a git repository
- **The file panel width can be dragged**
  - Drag the left edge of the file panel to 200–640 px; the width is remembered

### 🐛 Fixed

- **Claude Code no longer renders garbled, overlapping output**
  - Local sessions handle terminal resizes the Windows ConPTY way, so maximizing or enlarging the window no longer duplicates status lines
  - Resizes while dragging the window or file panel are merged and reported only when you stop; minimizing no longer shrinks the terminal to a tiny size
  - The terminal size is resynced when switching back to a session and when a hub takeover ends
- **Pressing `Tab` in Claude Code no longer inserts Tether's own command completion**
  - The completion popup no longer appears while Claude Code or a full-screen program is running
- **Tables in Claude Code output can be selected and copied**

## [0.5.0] - 2026-11-15

### Added

- With tether-hub 0.4.0: scroll back through history from before a takeover on the hub web page, paste / upload images to taken-over local sessions, and close sessions on this machine from the hub.

### Changed

- The hub protocol v2 gains capability negotiation (`caps` in hello / welcome); backward compatible with older hubs.

### Fixed

- Many Claude Code status fixes: idle reminders no longer show as "Waiting for confirmation", background tasks no longer cause false "Done" reports, slash commands and `/clear` no longer leave the status stuck, and informational notifications no longer trigger alerts.

## [0.4.1] - 2026-11-15

### Fixed

- Auto-resumed Claude Code conversations keep their original permission mode; the settings dialog header and buttons stay fixed while scrolling.

## [0.4.0] - 2026-11-15

### Added

- Optional connection to a self-hosted management server (tether-hub) that receives the session list, with optional remote takeover of sessions from the hub web page; "Test connection" for SSH hosts.

### Changed

- The host info bar moved below the terminal; SSH authentication errors are more specific.

## [0.3.0] - 2026-11-14

### Added

- "Latte Night" dark theme and a custom title bar.

### Changed

- New cream-style visual design with bundled Nunito / JetBrains Mono fonts; host info shown as a compact status bar.

## [0.2.0] - 2026-11-14

### Added

- Host info card with CPU, memory, GPU and network usage for the session's machine.

### Changed

- Smaller installer (3.9 MB → 2.7 MB).

## [0.1.1] - 2026-11-14

### Added

- Open-sourced under Apache-2.0; automatic updates from GitHub Releases that do not interrupt running sessions.

### Changed

- Simplified README with detailed docs moved to `docs/`; copying in the file tree also puts file names on the clipboard; SSH disconnect reasons are logged.

## [0.1.0] - 2026-11-13

First release.

### Added

- Multi-session terminal whose sessions are held by the background service `tetherd`, with notifications, Claude Code integration, SSH remote sessions, a file tree with transfers and Markdown preview, and detachable session windows.

[Unreleased]: https://github.com/tongxin-77/tether/compare/v0.7.1...HEAD
[0.7.1]: https://github.com/tongxin-77/tether/releases/tag/v0.7.1
[0.7.0]: https://github.com/tongxin-77/tether/releases/tag/v0.7.0
[0.6.0]: https://github.com/tongxin-77/tether/releases/tag/v0.6.0
[0.5.9]: https://github.com/tongxin-77/tether/releases/tag/v0.5.9
[0.5.8]: https://github.com/tongxin-77/tether/releases/tag/v0.5.8
[0.5.7]: https://github.com/tongxin-77/tether/releases/tag/v0.5.7
[0.5.6]: https://github.com/tongxin-77/tether/releases/tag/v0.5.6
[0.5.5]: https://github.com/tongxin-77/tether/releases/tag/v0.5.5
[0.5.4]: https://github.com/tongxin-77/tether/releases/tag/v0.5.4
[0.5.3]: https://github.com/tongxin-77/tether/releases/tag/v0.5.3
[0.5.2]: https://github.com/tongxin-77/tether/releases/tag/v0.5.2
[0.5.1]: https://github.com/tongxin-77/tether/releases/tag/v0.5.1
[0.5.0]: https://github.com/tongxin-77/tether/releases/tag/v0.5.0
[0.4.1]: https://github.com/tongxin-77/tether/releases/tag/v0.4.1
[0.4.0]: https://github.com/tongxin-77/tether/releases/tag/v0.4.0
[0.3.0]: https://github.com/tongxin-77/tether/releases/tag/v0.3.0
[0.2.0]: https://github.com/tongxin-77/tether/releases/tag/v0.2.0
[0.1.1]: https://github.com/tongxin-77/tether/releases/tag/v0.1.1
[0.1.0]: https://github.com/tongxin-77/tether/releases/tag/v0.1.0
