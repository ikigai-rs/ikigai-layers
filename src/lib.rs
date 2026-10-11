//! **A layer host.** A layer is one independent shared space between participants, named
//! `urn:layer:{layer}`: its definition (a file in the host's configuration) chooses an
//! ACCESS MODEL, names its closed SHACL admission shapes and its egress context, and its
//! grants are capabilities named for it (`urn:cap:layer:{verb}:{layer}`). It has its own
//! gapless feed. Adding a layer whose model exists is configuration, never code.
//!
//! A collaboration is a MANIFEST (`urn:collab:{name}`): which layers it mounts and which
//! participant holds which verbs on each. The door's enrollment (cli's `grants.json` and
//! `clients.json`) is generated from the manifests ([`grants`], and the `ikigai-layers
//! grants <config dir>` binary), so the manifest is the one thing an operator edits.
//!
//! This version builds the `channel` model: append-only entries, one graph per author.
//!
//! ```text
//! urn:layer:{layer}                 Source  the definition (ik:Layer)         read:{layer}
//! urn:layer:{layer}:shapes          Source  the closed admission shapes       read:{layer}
//! urn:layer:{layer}:context         Source  the egress context (JSON-LD)      read:{layer}
//! urn:layer:{layer}:feed            Source  events after= (wait= ≤ 60 s)      feed:{layer}
//! urn:layer:{layer}:say             Sink    append one entry, answer its IRI  say:{layer}
//! urn:layer:{layer}:entries         Source  entries after= from= type=        read:{layer}
//! urn:layer:{layer}:entry:{id}      Source  one entry (never changes)         read:{layer}
//! urn:layer:{layer}:graph:{slug}    Source  one author's entries              read:{layer}
//! urn:layer:{layer}:latest          Source  newest entry about= not superseded read:{layer}
//! urn:layers:feed                   Source  one wait over several feeds       feed:{each layer}
//! urn:layers:participants           Source  who each principal is             layers:read:participants
//! urn:collab:{name}                 Source  a manifest                        collab:read:{name}
//! ```
//!
//! (Capabilities abbreviated: `read:{layer}` is `urn:cap:layer:read:{layer}`.)
//!
//! # A host library
//!
//! No binary serves anything: a host (ikigai-cli's `serve`, ledger #1189) opens a [`Host`]
//! over its configuration and state directories and binds [`space`], beside
//! `ikigai-shacl` (admission composes `urn:shacl:validate`) and with a clock (every entry
//! is stamped with the kernel's time). The door mints each connection's principal; `say`
//! refuses a request that carries none.
//!
//! ```
//! use std::sync::Arc;
//! use ikigai_core::{Fallback, Kernel, Space, SystemClock};
//!
//! # let dir = std::env::temp_dir().join(format!("ikigai-layers-doc-{}", std::process::id()));
//! # let (config, state) = (dir.join("config"), dir.join("state"));
//! # std::fs::create_dir_all(&config).unwrap();
//! let host = ikigai_layers::Host::open(&config, &state).expect("a configuration that checks");
//! let root = Fallback::new(vec![
//!     Arc::new(ikigai_layers::space(&host)) as Arc<dyn Space>,
//!     Arc::new(ikigai_shacl::space()) as Arc<dyn Space>,
//! ]);
//! let kernel = Kernel::new(Arc::new(root)).with_clock(Arc::new(SystemClock));
//! # let _ = kernel;
//! # std::fs::remove_dir_all(&dir).unwrap();
//! ```
//!
//! # Isolation
//!
//! Separate channels, separate workflows, no overlap unless decided (Brian, 2026-10-10). A
//! layer belongs to ONE collaboration unless its definition says `shared = [..]` naming
//! every collaboration that mounts it; a manifest that breaks this is refused when the host
//! opens and when the generator runs, so no generated grant reaches another
//! collaboration's layer. See [`config::load_collabs`].
//!
//! # What this crate never writes
//!
//! `ik:layer` (lowercase) is a dry run's chain cell with `rdfs:domain ik:Consultation`;
//! writing it would type the subject a consultation. A layer is an `ik:Layer`, and the
//! layer an entry belongs to is the `{layer}` segment of its own IRI.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod channel;
pub mod config;
pub mod endpoints;
pub mod feed;
pub mod grants;
pub mod host;
pub mod id;
pub mod name;
pub mod rdf;
pub mod time;

pub use config::{Config, ConfigError, LayerDef, Manifest, Model};
pub use endpoints::space;
pub use host::Host;

/// The README's examples, compiled so they cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
