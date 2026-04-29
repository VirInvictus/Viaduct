# Viaduct: Comprehensive Architectural Review and 2.0 Evolution Plan

This document provides a deep-dive comparative analysis of three distinct codebases: the original Swift-based **NetNewsWire** (`.netnewswire`), the GTK-native **NewsFlash** (`.newsflash`), and this project, **Viaduct**. By examining their approaches to data modeling, state management, parsing, and UI paradigms, we identify Viaduct's path forward: a modern, idiomatic GTK4 application that remains a faithful, high-performance port of NetNewsWire.

---

## 1. Architectural Comparison

### NetNewsWire (`.netnewswire`)
**Tech Stack:** Swift, AppKit/UIKit, CoreData/SQLite
- **Architecture:** Classic Model-View-Controller (MVC) heavily reliant on Apple's native application frameworks. It is designed to be deeply integrated into the Apple ecosystem (macOS, iOS).
- **Data Models:** Deeply rooted in `RSDatabase` (SQLite) and highly tuned Swift structs. The feed and article abstractions are normalized, and the app leverages dedicated Swift modules (e.g., `RSParser`, `RSCore`).
- **State Management:** Driven primarily by Key-Value Observing (KVO), Apple's `NotificationCenter`, and standard Cocoa delegates.
- **UI Paradigms:** Relies on `WKWebView` for reading articles. It uses a robust template and macro engine to inject article data into HTML wrappers, rendering custom CSS themes perfectly.

### NewsFlash (`.newsflash`)
**Tech Stack:** Rust, GTK4, libadwaita
- **Architecture:** Highly modular GTK4 application leveraging the GObject type system.
- **Data Models:** Exposes business logic to GTK using custom `GObject` subclasses. This allows native GTK models (like `gio::ListStore`) to interface with Rust data.
- **State Management:** Employs `#[glib::derived_properties]` extensively. Property changes automatically trigger UI redraws via `gtk::Expression` bindings.
- **UI Paradigms:** 
  - **Declarative UI:** Uses **Blueprint** (`.blp`) for a clean, declarative UI structure.
  - **Native Rendering:** Diverges by using `html2gtk` for native widget rendering instead of WebKit. Paradoxically, while avoiding a browser engine, the resulting heavyweight GObject widget trees contribute to a much higher idle memory footprint (~600MB).

### Viaduct (Current Workspace)
**Tech Stack:** Rust, GTK4, libadwaita, WebKitGTK
- **Architecture:** Strict crate-level separation: `viaduct-core` (headless logic) and `viaduct` (GTK UI). It is a literal, functional translation of NetNewsWire's logic into Rust.
- **Data Models:** Uses pure Rust structs in the core, wrapped in lightweight glib wrappers for the UI.
- **State Management:** Highly imperative and centralized in `ViaductWindow`. UI updates are manually triggered, leading to boilerplate-heavy synchronization.
- **UI Paradigms:** 
  - **WebKit Fidelity:** Deliberately uses a neutered `WebKitWebView` to support NetNewsWire's CSS themes byte-for-byte. 
  - **Efficiency:** By leveraging a single WebKit instance and lean Rust models, Viaduct achieves a significantly lower memory footprint (~280MB idle) than its "native" Linux counterparts.
  - **Linux Integration:** Uses standard GTK tools but lacks the declarative cleanliness and reactive state seen in more mature GTK4 projects.

---

## 2. Areas of Improvement for Viaduct

Viaduct has successfully carried NetNewsWire across the gap. The 2.0 evolution focuses on making the Linux implementation as "naturally beautiful" and technically refined as the macOS original.

1. **Boilerplate Reduction:** The current GTK code is weighed down by manual widget manipulation. State changes (like unread counts or video detection) should drive the UI automatically.
2. **Monolithic Window Deconstruction:** `ViaductWindow` currently manages everything from sidebar badges to WebKit macro substitution. Modularizing this into self-contained components will improve maintainability.
3. **Async / UI Boundary Smoothing:** The manual channel-passing between `tokio` and GTK is verbose. A unified message-passing system will reduce the "clutter" of the 1.0 implementation.
4. **Refining the "Mac-Like" Feel on Linux:** While we use WebKit, there is room to improve how it integrates with the GTK chrome—better scrollbar styling, smoother transitions between articles, and tighter integration of GTK accents into the WebKit pane.

---

## 3. Future 2.0 Architectural Changes

The 2.0 plan is "Improvements-Only": enhancing the GTK implementation while strictly maintaining NetNewsWire parity and theme compatibility.

### A. Modular Componentization via Blueprint
Move away from monolithic `.ui` XML files. Viaduct will adopt **Blueprint** (`.blp`) to define its UI. `ViaductWindow` will be decomposed into discrete widgets:
- `SidebarView`: Encapsulates feed navigation and badges.
- `TimelineView`: Manages the article list and search states.
- `ArticlePaneView`: Manages the reading experience and WebKit lifecycle.
This modularity allows each part of the UI to be as polished as its macOS counterpart without polluting the global window state.

### B. Reactive State via `glib::derived_properties`
We will replace manual UI invalidation with native GObject properties. By exposing state (e.g., `is_starred`, `unread_count`) as GTK properties, we can use `gtk::Expression` to bind UI elements directly to data. This eliminates the "refresh" methods currently scattered throughout the codebase.

### C. Actor-Model Event Bus (Relm4 Pattern)
Introduce a central application loop where `viaduct-core` emits an `Event` stream. The GTK layer subscribes to this stream and updates its properties. This unidirectional data flow will dramatically simplify the async logic and reduce the reliance on complex closure cloning.

### D. Advanced WebKit Integration & Theme Fidelity
WebKit is the cornerstone of Viaduct's visual identity. The 2.0 changes will double down on this:
- **Encapsulated Renderer:** The WebKit logic will move into a dedicated `ArticleRenderer` GObject. This component will handle the NetNewsWire macro engine and CSS injection in isolation.
- **Visual Polish:** We will further refine the CSS bridge to ensure Linux system settings (like dark mode and accent colors) propagate into the WebKit pane even more seamlessly, achieving the "naturally beautiful" aesthetic of the macOS original while retaining full compatibility with the existing 8 themes.
- **Performance Preservation:** We will maintain the single-instance, locked-down WebKit architecture that keeps our memory usage at ~280MB, ensuring Viaduct remains the most efficient visual RSS reader on Linux.

By focusing on these structural and visual refinements, Viaduct 2.0 will fulfill its promise: the gold standard of NetNewsWire on Linux.