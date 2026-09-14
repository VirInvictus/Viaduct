# Attributions

Viaduct is a direct translation of the NetNewsWire architectural philosophy into the Linux ecosystem. We are deeply grateful to the original creators of NetNewsWire.

## Core Inspiration
- **NetNewsWire**: Original Mac and iOS application by [Brent Simmons](https://github.com/brentsimmons) and [Ranchero Software](https://github.com/Ranchero-Software). Licensed under the MIT License.

## Libraries and Software

Viaduct is built with the following open-source libraries:

### Rust Ecosystem
- **GTK4 (gtk-rs)**: Rust bindings for the GTK 4 library. (MIT License)
- **tokio**: A runtime for writing reliable asynchronous applications with Rust. (MIT License)
- **rusqlite**: Ergonomic bindings to SQLite. (MIT License)
- **reqwest**: An easy and powerful Rust HTTP Client. (MIT/Apache 2.0)
- **quick-xml**: High performance xml pull-reader/writer. (MIT License)
- **serde**: A framework for serializing and deserializing Rust data structures efficiently and generically. (MIT/Apache 2.0)
- **serde_json**: JSON serialization and deserialization for Rust. (MIT/Apache 2.0)
- **ammonia**: A crate for cleaning up HTML from untrusted sources. (MIT/Apache 2.0)
- **crossbeam-channel**: Multi-producer multi-consumer channels for message passing. (MIT/Apache 2.0)
- **anyhow**: A trait-object based error type for easy idiomatic error handling in Rust applications. (MIT/Apache 2.0)
- **thiserror**: A derive macro for the standard library's error trait. (MIT/Apache 2.0)
- **tracing**: A framework for application-level tracing. (MIT License)
- **url**: URL library for Rust. (MIT/Apache 2.0)
- **chrono**: Date and time library for Rust. (MIT/Apache 2.0)
- **md-5**: MD5 hash function. (MIT/Apache 2.0)
- **lru**: A LRU cache implementation. (MIT License)
- **readability**: A port of Mozilla's readability to Rust. (MIT License)
- **vir-gtk**: VirInvictus's own shared widget kit (portal dark/light, base stylesheet, rows and alert dialogs). MIT, consumed as a lock-pinned git dependency.
- **oo7**: Freedesktop Secret Service credential storage (the Inoreader account credentials). (MIT License)
- **ashpd**: xdg-desktop-portal client (Background portal for run-in-background). (MIT License)
- **ksni**: StatusNotifierItem system tray. (MIT License)
- **mimalloc**: Replacement allocator as a global `#[global_allocator]` in the binary crate. (MIT License)
- **tracing-subscriber**: Log emission/filtering on top of `tracing`. (MIT License)
- **WebKitGTK (webkit6)**: The Rust bindings and the WebKitGTK 6.0 library rendering the article pane. (LGPL-2.1+; bindings MIT/Apache)

## Bundled Fonts

Shipped in `data/fonts/` and installed at first run so the article pane can resolve them:

- **Inter** (Regular, Bold): Rasmus Andersson. SIL Open Font License 1.1.
- **Source Serif 4** (Regular, Bold): Adobe. SIL Open Font License 1.1.
- **JetBrains Mono** (Regular): JetBrains. SIL Open Font License 1.1.
- **Atkinson Hyperlegible** (WebKit-side bundle): Braille Institute of America. SIL Open Font License 1.1.

Each family's OFL copyright notice travels with the upstream release; the license texts ship beside the fonts in the next packaging pass (OFL requires the notice accompany redistributions).

## Other Components
- **NetNewsWire Logic & Assets**: Portions of the architectural logic and design principles are derived from the [NetNewsWire repository](https://github.com/Ranchero-Software/NetNewsWire).
