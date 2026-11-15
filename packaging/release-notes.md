**New in alpha 2:** the app has its own icon, on both systems.

An early public build of Sill Desktop, an unofficial desktop client for [Sill](https://getsill.app). It shows a Mac's windows, or its whole desktop, on another computer, and lets you use them with that computer's keyboard and mouse. The Mac runs the free Sill for Mac app, 0.6.1 or later.

**This is alpha software.** It streams and takes input in the author's own use, but most of it has only been tested against a synthetic host, on very few machines. Features are missing and it will have bugs. Please report what you find in Issues.

## Downloads

| System | File |
|---|---|
| Mac with Apple silicon, macOS 12 or later | `Sill-Desktop-…-macos-arm64.zip` |
| Windows 10 or 11, 64-bit Intel or AMD | `Sill-Desktop-…-windows-x64.zip` |

Linux, Intel Macs and Windows on ARM are coming.

Neither build is signed with a paid certificate yet, so the system asks you to confirm the first launch. The steps for both systems are in the [README](https://github.com/aminehmida/sill-desktop#installing). In short:

- **Mac:** unzip, move *Sill Desktop* to Applications, try to open it, then allow it in **System Settings › Privacy & Security › Open Anyway** (or run `xattr -dr com.apple.quarantine "/Applications/Sill Desktop.app"`).
- **Windows:** extract the whole folder, run *Sill Desktop.exe*, and on the SmartScreen notice choose **More info › Run anyway**.

## What is there

Built, and passing an automated end-to-end run on both systems; not all of it has been used against a real Mac yet.

- Pairing with a Mac by address and code, and with a Mac found on the same network.
- A window on its own filling this app's window as if it ran here, or the whole Desktop under a bar with live window thumbnails.
- Keyboard (by key position, as a keyboard plugged into the Mac), mouse, scrolling, the Mac's pointer shape.
- The Mac's sound, the clipboard both ways, the Mac's menu bar, text size, launching apps, stream settings.

## Known gaps

- On Windows the picture is decoded in software so far; hardware decoding is untested.
- The Windows key and Alt+Tab stay with Windows.
- No trackpad gestures, no input methods (IME) for typing, no reordering of thumbnails, no first-run tour.
- No installer and no automatic updates.

Sill is Noah Saffer's app; this client is not made or supported by him.
