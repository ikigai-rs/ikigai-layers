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
//! This version builds two access models: `channel` (append-only entries, one graph per
//! author) and `tuplespace` (offers taken atomically, reports addressed to their offerer,
//! with receipts; see [`tuplespace`]).
//!
//! On those two, a GATEWAY between kernels is configuration plus a reference participant
//! ([`gateway`]): an ask layer per asker carries requests and their answers (a representation,
//! or a typed error), and an event channel per resource owner carries invalidations.
//!
//! Every answer a participant reads also has a JSON face ([`json`]), so a participant with
//! only an HTTP client and a JSON parser can take part: a graph as the layer's egress context,
//! pruned and compacted, always one top-level `@graph` array (`as=application/ld+json`), and a
//! list or a write's answer as plain JSON (`as=application/json`).
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
//! urn:layer:{layer}:tuples          Sink    out: drop one tuple, its subject   out:{layer}
//!                                   Source  rd: pending tuples, or receipts    read:{layer}
//!                                   Delete  take: one tuple, atomically        take:{layer}
//! urn:layer:{layer}:receipt:{id}    Source  one take (never changes)           read:{layer}
//! urn:layers:feed                   Source  one wait over several feeds       feed:{each layer}
//! urn:layers:participants           Source  who each principal is             layers:read:participants
//! urn:layers:me                     Source  who the door says the caller is   collab:read:* (any grant)
//! urn:collab:{name}                 Source  a manifest                        collab:read:{name}
//! urn:collab:{name}:participants    Source  who takes part in it              collab:read:{name}
//! ```
//!
//! (Capabilities abbreviated: `read:{layer}` is `urn:cap:layer:read:{layer}`.)
//!
//! # Two builds: the host, and a participant
//!
//! The `host` feature, on by default, is the layer host: [`Host`], the layer space a door
//! binds, the configuration, both access models, the feeds, the JSON faces and the enrollment
//! generator. A PARTICIPANT, a tool that only talks to a layer door (the [`gateway`]'s `Asker`
//! and `Resolver` over QUIC, say), takes the crate with `default-features = false` and gets the
//! modules that need no host: [`gateway`], [`name`], [`rdf`], [`id`] and [`time`]. That build
//! links no store, no space, no validator and no JSON-LD processor, so no oxigraph and no
//! RocksDB (ledger #1403).
//!
//! ```toml
//! [dependencies]
//! ikigai-layers = { version = "0.3.1", default-features = false }
//! ```
//!
//! # A host library
//!
//! No binary serves anything: a host (ikigai-cli's `serve`, ledger #1188) opens a [`Host`]
//! over its configuration and state directories and binds [`space`] with a clock (every
//! entry is stamped with the kernel's time). The door mints each connection's principal;
//! `say`, `out` and `take` refuse a request that carries none.
//!
//! **The layer space is all a door binds.** Admission validates with ikigai-shacl, which
//! this crate links and calls itself, so the door's kernel needs no `urn:shacl:validate`,
//! and binding one beside would offer it to every participant: it declares no capability
//! (ledger #1266). **One host per state directory**: [`Host::open`] locks it, and refuses
//! while another host holds it (ledger #1267). **State is sealed at rest**: every file the
//! host writes is age ciphertext sealed to the host's own key, an identity file handed to
//! [`Host::open`] as a path ([`at_rest`], ledger #2011).
//!
//! ```
//! # #[cfg(feature = "host")]
//! # fn main() {
//! use std::sync::Arc;
//! use ikigai_core::{Kernel, SystemClock};
//! use ikigai_layers::StateKey;
//!
//! # let dir = std::env::temp_dir().join(format!("ikigai-layers-doc-{}", std::process::id()));
//! # let (config, state) = (dir.join("config"), dir.join("state"));
//! # std::fs::create_dir_all(&config).unwrap();
//! # let identity = ikigai_encrypt::age::Identity::generate();
//! # std::fs::write(dir.join("state.key"),
//! #     ikigai_encrypt::age::ExposeSecret::expose_secret(&identity.to_string())).unwrap();
//! // An age identity file (`age-keygen -o state.key`), read once, at open.
//! let key = StateKey::load(dir.join("state.key")).expect("an age identity");
//! let host = ikigai_layers::Host::open(&config, &state, &key).expect("a configuration that checks");
//! let kernel = Kernel::new(Arc::new(ikigai_layers::space(&host))).with_clock(Arc::new(SystemClock));
//! // A second host over the same state directory is refused while this one lives.
//! assert!(ikigai_layers::Host::open(&config, &state, &key).is_err());
//! # drop(kernel);
//! # drop(host);
//! # std::fs::remove_dir_all(&dir).unwrap();
//! # }
//! # #[cfg(not(feature = "host"))]
//! # fn main() {}
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
// The crate's documentation describes the host, and links its items. Built without `host`
// (a participant), those items do not exist, so the links have nowhere to go: allowed in that
// build only. The default build, which `cargo doc` in the shared CI runs with `-D warnings`,
// still refuses every broken link, in every module.
#![cfg_attr(not(feature = "host"), allow(rustdoc::broken_intra_doc_links))]

#[cfg(feature = "host")]
pub mod at_rest;
#[cfg(feature = "host")]
pub mod channel;
#[cfg(feature = "host")]
pub mod config;
#[cfg(feature = "host")]
pub mod endpoints;
#[cfg(feature = "host")]
pub mod feed;
pub mod gateway;
#[cfg(feature = "host")]
pub mod grants;
#[cfg(feature = "host")]
pub mod host;
pub mod id;
#[cfg(feature = "host")]
pub mod json;
pub mod name;
pub mod onboard;
pub mod rdf;
pub mod time;
#[cfg(feature = "host")]
pub mod tuplespace;

#[cfg(feature = "host")]
pub use at_rest::{Migration, StateKey};
#[cfg(feature = "host")]
pub use config::{Config, ConfigError, LayerDef, Manifest, Model};
#[cfg(feature = "host")]
pub use endpoints::space;
#[cfg(feature = "host")]
pub use host::Host;

/// The README's examples, compiled so they cannot rot. They open a host, so they run with it.
#[cfg(all(doctest, feature = "host"))]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
