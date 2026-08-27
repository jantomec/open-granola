# Releasing (macOS)

As-built process for v0.2.0 (2026-08-27, `c2acf56`) and v0.2.1 (2026-08-27, `73e255a`).

## Build + sign

```bash
APPLE_SIGNING_IDENTITY="Developer ID Application: <name> (<team id>)" \
  npm run tauri build -- --bundles dmg
# → src-tauri/target/release/bundle/dmg/Open Granola_<ver>_aarch64.dmg
```

- The identity must be a **Developer ID Application** cert in the login keychain
  (`security find-identity -v -p codesigning` to list). Passing it via env keeps
  personal cert names out of tauri.conf.json.
- Versions live in three places and must agree: `package.json`,
  `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json` — plus the hardcoded footer
  string in `src/components/SettingsView.tsx`.
- **Not notarized** (decision from v0.2.0: skipped; no notarytool credentials stored).
  Downloaders get Gatekeeper's "Apple could not verify…" and must use
  System Settings → Privacy & Security → Open Anyway. Release notes must say so.
  To notarize later: `xcrun notarytool store-credentials`, then submit + staple + re-upload.
- **DMG bundling can hang forever**: `bundle_dmg.sh` drives Finder via AppleScript.
  If it hangs, kill it, `hdiutil detach /Volumes/dmg.*`, and rerun (or build
  `--bundles app` and make the DMG separately). Rust compile itself is ~75 s
  incremental; anything much longer is the Finder hang.

## Verify the artifact (not the pipeline)

Mount the DMG and check the .app inside it — the exact bytes users get:

1. `codesign --verify --deep --strict -v "<app>"`
2. `codesign -d --entitlements - "<app>"` → must show ONLY
   `com.apple.security.device.audio-input` and
   `com.apple.security.personal-information.calendars`.
   **No app-sandbox** (white-screens the webview — see entitlements.plist),
   **no network.client** (Airlock).
3. Launch the binary directly with `RUST_LOG=info`, stderr to a file. Healthy run:
   "airlock engaged", **zero** "webview reloaded" lines (a reload loop = WebContent
   crash = white screen), no seatbelt rc=-1 error.
4. Icon: `Contents/Resources/icon.icns` exists and `CFBundleIconFile` is set.

## Publish

```bash
git tag -a v<ver> -m "..." && git push origin v<ver>
cp "…/Open Granola_<ver>_aarch64.dmg" OpenGranola_<ver>_aarch64.dmg  # GitHub mangles spaces
gh release create v<ver> --repo jantomec/open-granola \
  "OpenGranola_<ver>_aarch64.dmg#Open Granola <ver> (macOS, Apple Silicon)" \
  --title "…" --notes-file notes.md
```

- `gh` resolves the default repo to **upstream** (`anshuman-pandey/...`); always pass
  `--repo jantomec/open-granola`.
- Close the loop: `gh release download` the asset and diff its SHA-256 against the
  local file.
- Include in the notes: Apple-Silicon-only, macOS 14.4+, the Open Anyway steps, and
  the manual model table (path: `~/Library/Application Support/app.opengranola/library/models/`).

## TCC facts worth remembering

- System-audio capture (the process tap) prompts only for a process with a bundle
  identity carrying `NSAudioCaptureUsageDescription`; a bare CLI is **denied silently**
  (exact-zero samples, no prompt). `examples/tap_smoke.rs` documents the .app-wrapper
  recipe. `NSSystemAudioUsageDescription` does not exist — that key was a v0.1 bug.
- Tauri embeds `src-tauri/Info.plist` into dev binaries too, so dev builds prompt fine.
