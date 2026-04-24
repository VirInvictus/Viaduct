// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! viaduct library root. The binary (`src/main.rs`) is a thin wrapper that
//! spins up tracing + tokio + the GTK application; everything else lives here
//! so auxiliary binaries (profiling harnesses, future CLI tools) can share
//! the same modules.

pub mod database;
pub mod error;
pub mod models;
pub mod network;
pub mod parser;
pub mod paths;
pub mod ui;
