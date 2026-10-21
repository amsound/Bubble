# Platform audit (egui app, 2026-10-21)

A review of how Bubble works with Windows and macOS, prompted by the Windows Open dialog
not appearing. Most findings are about operating-system integration rather than egui, so they
carry straight over to the Slint port: each one says what to do in Bubble today and what the
Slint version must do from the start.

Severity: **High** can lose work or stop a show; **Medium** confuses or blocks an operator;
**Low** is polish.

## Windows and dialogs

| # | Finding | Severity | Where | Fix now | In Slint |
|---|---|---|---|---|---|
| W1 ✅ | File dialogs (Open, Assign, Relink, Choose Folder) are created without a parent window. Windows can open them behind Bubble's (often maximised) window, so they seem not to appear. | High | `crates/bubble-app/src/dialogs.rs` `launch` | Pass the main window to `rfd::AsyncFileDialog::set_parent`, so each dialog is owned by Bubble and shown on top of it. Same for the error `MessageDialog` in `main.rs`. | Same: always set the parent window on every native dialog. |
| W2 | Cart Setup, Outputs, Session/Page Settings, Log, Diagnostics and Reassign are separate top-level windows with no owner (egui cannot set one). On Windows each gets its own taskbar button and falls behind the main window when it is clicked; on macOS they can hide behind it. Choosing them again now raises them (D-36), but they still get lost. | Medium | `crates/bubble-app/src/native.rs` `native_window` | Interim: keep raising on reopen. | Make settings, quick edit and Cart Setup panels or sheets inside the main window. Use separate windows only where a second screen helps (Speed Bar, maybe Cart Setup), owned by the main window. |
| W3 | Windows are fixed-size and sized for macOS fonts and metrics. Content clips or wraps on Windows (Segoe UI, other scale factors). | Medium | `WindowSpec` sizes across `app.rs` and `app/` | Fit-to-content (already used for small windows) everywhere. | Layout-driven sizes (Slint layouts), tested at 100/125/150/200% scale on Windows. |
| W4 | The title bar stays light in dark mode on Windows (Bubble's own appearance setting is not passed to the window). | Low | `main.rs` viewport | Set the window theme from the Appearance preference. | Same. |
| W5 | The in-window menu bar (Windows) has no Alt-key access (Alt, then F for File). | Low | `menu.rs` | — | Slint `MenuBar`, check keyboard access. |

## Files and sessions

| # | Finding | Severity | Where | Fix now | In Slint |
|---|---|---|---|---|---|
| F1 ✅ | Saving replaces the session file by renaming a temporary file over it, with no retry. On Windows, antivirus, OneDrive or the search indexer often hold the file for a moment, so the rename fails with "Access is denied" and the save fails. | High | `crates/bubble-project/src/io.rs` `write_atomic` | Retry the replace a few times over ~1 s on Windows before reporting failure. | Shared crate: fixed once for both. |
| F2 ✅ | The default session folder is Documents, which is cloud-synced by default on both systems (OneDrive on Windows, iCloud on macOS). Files can become download-on-demand placeholders; with disk streaming, playback could stall while one downloads. | High | `session_ui.rs` `default_root` | Default to a local folder outside the synced ones, and warn when a session is opened from a synced folder (detect OneDrive paths, and macOS "dataless" files). Advise "Keep Downloaded"/"Always keep on this device". | Same, shared. |
| F3 ✅ | Failures to save preferences, autosaves and crash reports are ignored silently (21 places, 12 of them preference saves). If autosave keeps failing, the operator believes they are protected when they are not. | High | `app.rs`, `app/*.rs` (`let _ = …save()`) | Log every failure; show a status warning when autosave or saving preferences fails. | Same, shared. |
| F4 ✅ | Two copies of Bubble can run at once (double-clicking twice while Windows Defender scans, for instance). They compete for audio devices and write the same session and autosave. | Medium | `main.rs` | A single-instance guard: a second launch hands its file to the running copy and brings it to the front. | Same. |
| F5 | macOS: double-clicking a session, Open With, or dropping a file on the Dock icon does nothing ("cannot open files of this type"). There are no document types in the bundle and no handler for the system's open-file request. Windows now opens a file passed at launch (D-36), but no file types are registered. | Medium | `scripts/release.sh` Info.plist; app | Declare `.bubble`, `.dta` and `.pkg` in `CFBundleDocumentTypes` (SpotOn's `.pkg` as Viewer only), and handle the open-file event. Windows: register types in an installer. | Same; Slint's winit backend needs the same open-file hook. |
| F6 | A file name or session path over 260 characters on Windows, or on a network share (UNC), is untested. | Low | `crates/bubble-project/src/resolve.rs` | Add tests for UNC and long paths. | Shared. |

## Start-up, windows and devices

| # | Finding | Severity | Where | Fix now | In Slint |
|---|---|---|---|---|---|
| S1 ✅ | The audio device list is read on the interface thread at start-up and when Outputs opens or rescans. Windows device enumeration can take seconds (Bluetooth, Dante, virtual devices), freezing the interface. | Medium | `app.rs` `new`, `app/outputs_ui.rs` | Enumerate on a background thread (as the device watcher already does). | Same, shared. |
| S2 ✅ | A restored window position is not checked against the connected screens. After unplugging a second monitor, the window can open off-screen. | Medium | `main.rs`, `app.rs` `track_window` | On the first frame, if the window is not on any monitor, centre it on the primary one. | Same. |
| S3 | Unsigned app: Windows SmartScreen and Defender delay the first launch (minutes on a slow machine); macOS Gatekeeper blocks it until allowed. | Medium | release | Code-signing certificate (Windows) and Developer ID with notarisation (macOS). | Same. |
| S4 | No Windows installer: no Start menu entry, file types or uninstaller, and the exe runs from wherever it was unzipped. | Low | release | An installer (MSI or Inno Setup) once signing exists. | Same. |
| S5 | Adding files reads part of each file to fingerprint it (first and last 1 MiB) on the interface thread. Slow from a network or cloud folder. | Low | `bubble-control` `retarget_assets`, `make_asset_ref` | Fingerprint on the job thread. | Shared. |

## Input

| # | Finding | Severity | Where | Fix now | In Slint |
|---|---|---|---|---|---|
| I1 | Touch has no right-click: press-and-hold opens quick edit only in Edit mode (D-36). | Medium | `app.rs` cart wall | — | Press-and-hold everywhere (prototype already does), tap on release in Edit mode. |
| I2 | Hot keys use physical key positions (right for a fixed layout), but tiles label them with the US key name, so on AZERTY a key placed where US has Q shows "Q" although the key says "A". | Low | `bubble-control` `describe` | Label with the key's character on the current layout. | Same. |
| I3 | Dropping files uses the system cursor position (`mouse_position` crate) because egui does not report where a drop lands. | Low | `app.rs` `drop_files` | — | Slint needs the winit window-event hook for OS file drops (unstable API); keep drop targeting exact. |
| I4 | Hot keys are suspended while a text field has focus, through egui's own check. | — | `app.rs` `keyboard` | Works. | Re-create explicitly: no cart triggers while typing, Escape still stops. |

## Engine (checked, no change needed)

- No panics on the audio thread: the two `expect`s in `mixer.rs`/`stream.rs` cannot fail as
  written (a voice pool is never empty; the decoder is set just before).
- The audio thread is real-time on Windows (MMCSS) and macOS; the disk reader keeps 4 s ahead.
- Device loss falls back to the default output and reconnects (D-32).

## Fixed (2026-10-21)

✅ marks items fixed in the egui app:

- **W1** every file dialog is owned by the main window.
- **F1** replacing a saved file retries for about a second when another program holds it.
- **F2** new sessions default to `~/Bubble`, and a session in a synced folder shows a warning (a clear alert if any audio is online-only), re-checked every minute.
- **F3** failed autosaves and preference saves are logged and shown in red in the status bar until one succeeds.
- **F4** a second launch hands its file to the running copy, which comes to the front.
- **S1** the device list is read on a helper thread. Opening the audio streams at start-up still enumerates devices on the main thread.
- **S2** on Windows, a saved window position off every connected screen is ignored. macOS moves windows back onto a screen itself.

## Order

1. Before the next release: W1, F1, F3, F2 (warning at least), F4, S1, S2.
2. With the Slint port: W2, W3, I1, I3, I4, F5, W4, W5.
3. When there is budget: S3 and S4 (signing, installer), then F6, S5, I2.
