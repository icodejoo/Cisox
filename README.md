# Snow Apps

Snow Apps repository, providing source code for Snow Shot and Snow Image Viewer.

<div style="font-size: 128px">🏗️🚧🦺</div>

## Install Snow Shot on Windows

Download the installer from [Snow Shot releases](https://github.com/mg-chao/snow-apps/releases).
Install the offline Windows x64 package from the WinGet community repository with:

```powershell
winget install --exact --id mg-chao.snow-shot --source winget
```

This package includes published beta versions and the default OCR resources. See
[WinGet release support](docs/snow-shot-releases.md#winget) for upgrades, removal,
and maintainer setup.

## Install Snow Shot on macOS

After the first stable Homebrew release is published, Apple Silicon Macs running
macOS 15 or later can install with:

```sh
brew update
brew install --cask mg-chao/tap/snow-shot
brew upgrade --cask snow-shot
brew uninstall --cask snow-shot
```

Installation reuses Snow Shot's local signing identity and may request Keychain
access and macOS privacy permissions. See [Homebrew installation and migration](docs-macos-build.md#homebrew-installation)
for existing installations, custom app directories, and recovery.

## Translating the interface

Cisox currently ships two interface languages: English (`en-US`, the source language) and Simplified Chinese (`zh-CN`). Messages that are missing in a language fall back to English, so a partly translated language still works.

### Add a new interface language

Everything lives in `snow-shot-rs/crates/snow-i18n/locales/`. A new language is only a new folder; no Rust code changes.

1. Copy `locales/en-US/` to `locales/<code>/`, where `<code>` is a language-region code such as `ja-JP`.
2. Translate the text on the right of each `=` in the `.ftl` files. Keep the message IDs and placeholders such as `{ $arg1 }` untouched, and keep the product name out of translated text (it is injected as `{ $product }`).
3. Edit `locales/<code>/locale.toml`:

   ```toml
   code = "ja-JP"                 # must equal the folder name
   native_name = "日本語"          # shown in the language dropdowns
   aliases = ["ja_JP", "ja"]      # other spellings that select this language
   system_prefixes = ["ja"]       # system language tags that start with these pick this language
   ```

4. Rebuild. `build.rs` scans `locales/*/` and embeds every `.ftl` and `locale.toml` automatically. The language then appears in Settings, Interface, Language (stored in the config as `ja_JP`), and is chosen on first start when it matches the system language. Without a saved choice Cisox uses the system language, and English if that is not supported.

Adding a new `.ftl` file to an existing language folder needs no code change either.

### Translate dropdown options and setting names

- Setting names and descriptions: `settings_items.ftl`, IDs `setting-<key>` and `setting-<key>-desc`, where `/` and `_` in the config key become `-` (for example `setting-screenshot-image-quality`).
- Dropdown options: `settings_options.ftl`, ID `setting-option-<key>-<value>`, lowercased with every non-alphanumeric character in the key and value turned into `-` (for example `setting-option-tray-icon-dark`). File and codec names that read the same in every language (`mp4`, `gif`, `apng`, `jxl`) are shown as is and need no entry.
- Group titles and the window text: `settings_ui.ftl`.
- Language names in the language dropdowns are always shown in their own language (`native_name` from `locale.toml`, plus a small built-in list for translate-only languages such as Japanese) and are not translated.
- The translation source and target language lists are the languages the translation engine supports. They are separate from interface languages, so adding an interface language does not change them.

### Check that a translation is complete

Run from `snow-shot-rs/`:

```powershell
# every language must have every message that en-US has
cargo run -q -p snow-i18n --bin snow-i18n-tool -- check crates/snow-i18n/locales
# every message ID referenced from source code must exist in en-US
cargo run -q -p snow-i18n --bin snow-i18n-tool -- extract crates --exclude crates/snow-i18n --strict-refs
# unit tests, including "every setting and dropdown option resolves in every language"
cargo test -p snow-i18n
cargo test -p snow-shot -- settings_text language_names
```

The first command lists the missing messages per language. A language that fails it still runs (missing messages show in English), but CI expects complete translations.

## Open Source Licenses

This is a multi-license repository. See [LICENSE.md](LICENSE.md) for the
repository-level scope rules and third-party material policy.

| Project | License |
| --- | --- |
| `ant_design_qt/` | [Apache License 2.0](ant_design_qt/LICENSE) |
| `snow-crates/` | [Apache License 2.0](snow-crates/LICENSE) |
| `snow_draw_engine_qt/` | [Apache License 2.0](snow_draw_engine_qt/LICENSE) |
| `snow_rust_ffi/` | [Apache License 2.0](snow_rust_ffi/LICENSE) |
| `snow_image/` | [GNU GPL v3.0 or later](snow_image/COPYRIGHT) |
| `snow_image_viewer/` | [GNU GPL v3.0 or later](snow_image_viewer/COPYRIGHT) |
| `snow_shot/` | [GNU GPL v3.0 or later](snow_shot/COPYRIGHT) |

Synchronized and bundled third-party materials retain their upstream licenses.
See [Ant Design Qt third-party notices](ant_design_qt/THIRD_PARTY_NOTICES.md)
and [Snow Shot third-party notices](snow_shot/THIRD_PARTY_NOTICES.md).
