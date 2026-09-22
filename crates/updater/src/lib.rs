// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The launcher's parts, as a library.
//!
//! The binary is three dozen lines of argument handling over this; the split
//! exists so the archive extractor can be fuzzed (charter rule 14) and the
//! install logic tested without starting a game.

pub mod archive;
pub mod install;
