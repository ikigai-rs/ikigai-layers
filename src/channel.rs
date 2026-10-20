//! The `channel` model: append-only entries, one graph per author.
//!
//! An entry is ONE subject, written by `say` and never changed again:
//!
//! - the author sends a graph about `<urn:layer:new>` (or one blank node) and nothing else;
//! - the host names it `urn:layer:{layer}:entry:{id}` and stamps who said it
//!   (`prov:wasAttributedTo`, the principal its door minted), when (`dcterms:created`, the
//!   kernel clock) and its place in the feed (`ik:seq`), refusing any of the three if sent;
//! - the stamped graph must conform to the layer's closed shapes;
//! - it is written once to `<state>/{layer}/graphs/{author slug}/{id}.ttl`, sealed at rest
//!   ([`crate::at_rest`]: the file is age ciphertext of the Turtle), and never rewritten
//!   (but for [`crate::Migration::Reseal`], which seals it again with the same Turtle): there is no edit and no delete, and a correction is a new entry that
//!   `ik:supersedes` the old one (the same author's, in the same layer).
//!
//! This module holds the entries and their index; the endpoints in [`crate::endpoints`]
//! compose admission (the host's own call to ikigai-shacl's validator) around it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use ikigai_core::Error;
use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use oxrdfio::{RdfFormat, RdfParser};

use crate::at_rest::{self, Sealer};
use crate::config::LayerDef;
use crate::id;
use crate::name::Participant;
use crate::rdf::{self, node, object_iri, subject_iri};

/// The subject an author writes for the entry the host is about to name (defined in
/// [`crate::name`], where a participant built without the host links it too).
pub use crate::name::NEW_SUBJECT;

/// The suffix of an entry file still being written (`.{id}.ttl.partial`, beside where it
/// will be stored): never read as an entry, and cleared on open.
pub const PARTIAL: &str = ".partial";

/// What the index knows about one entry.
#[derive(Clone, Debug)]
pub struct EntryMeta {
    /// Its feed sequence number.
    pub seq: u64,
    /// Its id (the last segment of its IRI).
    pub id: String,
    /// `urn:layer:{layer}:entry:{id}`.
    pub iri: String,
    /// Who said it (the principal).
    pub author: String,
    /// The author's graph name.
    pub slug: String,
    /// When, as `xsd:dateTime`.
    pub created: String,
    /// Its `rdf:type`s.
    pub types: BTreeSet<String>,
    /// The objects of the layer's `about` predicate.
    pub about: BTreeSet<String>,
    /// The entries it supersedes.
    pub supersedes: BTreeSet<String>,
}

/// A channel layer's entries.
pub struct Channel {
    sealer: Sealer,
    layer: String,
    about: String,
    graphs: PathBuf,
    entries: Vec<EntryMeta>,
    by_id: HashMap<String, usize>,
    superseded: HashSet<String>,
}

/// The prefix of every entry IRI in `layer`.
pub fn entry_prefix(layer: &str) -> String {
    format!("urn:layer:{layer}:entry:")
}

pub(crate) fn invalid(detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: "content".into(),
        detail: detail.into(),
    }
}

impl Channel {
    /// Open the entries under `dir` (`<state>/{layer}`), each opened with `sealer`,
    /// rebuilding the index from the files themselves: they are the record, and the index is
    /// only ever derived.
    pub fn open(def: &LayerDef, dir: &Path, sealer: &Sealer) -> Result<Channel, String> {
        let graphs = dir.join("graphs");
        at_rest::private_dir(&graphs)?;
        let mut channel = Channel {
            sealer: sealer.clone(),
            layer: def.name.clone(),
            about: def.about.clone(),
            graphs: graphs.clone(),
            entries: Vec::new(),
            by_id: HashMap::new(),
            superseded: HashSet::new(),
        };
        let mut found = Vec::new();
        for slug_dir in read_dir(&graphs)? {
            if !slug_dir.is_dir() {
                continue;
            }
            for file in read_dir(&slug_dir)? {
                if file.extension().is_none_or(|x| x != "ttl") {
                    // An entry write that never completed (see `append`): never a record.
                    let partial = file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with('.') && n.ends_with(PARTIAL));
                    if partial {
                        let _ = std::fs::remove_file(&file);
                    }
                    continue;
                }
                let text = channel.sealer.read_text(&file)?;
                let triples =
                    rdf::parse_turtle(&text).map_err(|e| format!("{}: {e}", file.display()))?;
                let meta = channel
                    .meta(&triples)
                    .map_err(|e| format!("{}: {e}", file.display()))?;
                let expected = slug_dir.join(format!("{}.ttl", meta.id));
                if expected != file {
                    return Err(format!(
                        "{}: holds {} by {}, which belongs at {}",
                        file.display(),
                        meta.iri,
                        meta.author,
                        expected.display()
                    ));
                }
                found.push(meta);
            }
        }
        found.sort_by_key(|m| m.seq);
        for meta in found {
            channel.index(meta)?;
        }
        Ok(channel)
    }

    fn index(&mut self, meta: EntryMeta) -> Result<(), String> {
        if self.by_id.contains_key(&meta.id) {
            return Err(format!("{} is stored twice", meta.iri));
        }
        self.superseded.extend(meta.supersedes.iter().cloned());
        self.by_id.insert(meta.id.clone(), self.entries.len());
        self.entries.push(meta);
        Ok(())
    }

    /// Read an entry's facts from its stored (stamped) graph.
    fn meta(&self, triples: &[Triple]) -> Result<EntryMeta, String> {
        let prefix = entry_prefix(&self.layer);
        let mut subject = None;
        for t in triples {
            let s = subject_iri(&t.subject).ok_or("a blank-node subject")?;
            match subject {
                None => subject = Some(s.to_string()),
                Some(ref one) if one != s => return Err("more than one subject".into()),
                _ => {}
            }
        }
        let iri = subject.ok_or("no entry")?;
        let id = iri
            .strip_prefix(&prefix)
            .filter(|id| id::is_id(id))
            .ok_or_else(|| format!("{iri} is not an entry of layer `{}`", self.layer))?
            .to_string();
        let (mut author, mut created, mut seq) = (None, None, None);
        let (mut types, mut about, mut supersedes) =
            (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        let seq_p = rdf::ik_seq();
        let supersedes_p = rdf::ik_supersedes();
        for t in triples {
            let p = t.predicate.as_str();
            if p == rdf::PROV_ATTRIBUTED {
                author = object_iri(&t.object).map(str::to_string);
            } else if p == rdf::DCT_CREATED {
                if let Term::Literal(l) = &t.object {
                    created = Some(l.value().to_string());
                }
            } else if p == seq_p {
                if let Term::Literal(l) = &t.object {
                    seq = l.value().parse::<u64>().ok();
                }
            } else if p == rdf::RDF_TYPE {
                types.extend(object_iri(&t.object).map(str::to_string));
            } else if p == supersedes_p {
                supersedes.extend(object_iri(&t.object).map(str::to_string));
            }
            if p == self.about {
                about.extend(object_iri(&t.object).map(str::to_string));
            }
        }
        let author = author.ok_or("no prov:wasAttributedTo")?;
        let slug = Participant::parse(&author)?.slug().to_string();
        let seq = seq.ok_or("no ik:seq")?;
        if id::seq_of(&id) != Some(seq) {
            return Err(format!("{iri} carries ik:seq {seq}, which its id does not"));
        }
        Ok(EntryMeta {
            seq,
            id,
            iri,
            author,
            slug,
            created: created.ok_or("no dcterms:created")?,
            types,
            about,
            supersedes,
        })
    }

    /// The highest sequence number among the stored entries (0 for none).
    pub fn last_seq(&self) -> u64 {
        self.entries.last().map_or(0, |m| m.seq)
    }

    /// Every entry, in sequence order.
    pub fn entries(&self) -> &[EntryMeta] {
        &self.entries
    }

    /// One entry, by id.
    pub fn get(&self, id: &str) -> Option<&EntryMeta> {
        self.by_id.get(id).map(|i| &self.entries[*i])
    }

    /// The newest entry whose `about` names `iri` and that no entry supersedes.
    pub fn latest(&self, iri: &str) -> Option<&EntryMeta> {
        self.entries
            .iter()
            .rev()
            .find(|m| m.about.contains(iri) && !self.superseded.contains(&m.iri))
    }

    /// The stored Turtle of one entry.
    pub fn turtle(&self, meta: &EntryMeta) -> Result<String, String> {
        self.sealer.read_text(&self.path_of(meta))
    }

    /// Seal again, to the current identity, every entry that is plaintext or sealed only to
    /// an older identity ([`crate::Migration::Reseal`]), each replaced whole. Answers how many.
    pub(crate) fn reseal(&self) -> Result<usize, String> {
        let mut resealed = 0;
        for meta in &self.entries {
            let path = self.path_of(meta);
            let bytes =
                std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            if self.sealer.is_current(&bytes) {
                continue;
            }
            let opened = self.sealer.open(&bytes).map_err(|why| why.about(&path))?;
            at_rest::replace(&path, &self.sealer.seal(&opened)?)?;
            resealed += 1;
        }
        Ok(resealed)
    }

    fn path_of(&self, meta: &EntryMeta) -> PathBuf {
        self.graphs
            .join(&meta.slug)
            .join(format!("{}.ttl", meta.id))
    }

    /// Store a stamped, admitted entry: written whole and synced under a name the index never
    /// reads ([`PARTIAL`]), then renamed into place, never over an existing entry; then
    /// indexed.
    ///
    /// So an entry file is whole or absent (ledger #1221). A write that fails part-way
    /// removes what it began, and if that removal fails too, or the host stops mid-write,
    /// what is left is a `.partial` file that the next open ignores and clears, never a
    /// torn entry it would refuse the layer over. Nothing carrying the entry's number was
    /// stored, so the next write takes that number.
    pub fn append(&mut self, triples: &[Triple]) -> Result<EntryMeta, String> {
        let meta = self.meta(triples)?;
        let path = self.path_of(&meta);
        let dir = path.parent().expect("a graph directory");
        at_rest::private_dir(dir)
            .map_err(|e| format!("create the graph of {}: {e}", meta.author))?;
        let sealed = self.sealer.seal(rdf::to_turtle(triples).as_bytes())?;
        let partial = dir.join(format!(".{}.ttl{PARTIAL}", meta.id));
        let written = at_rest::private_file()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&partial)
            .and_then(|mut file| {
                file.write_all(&sealed)?;
                file.sync_all()
            })
            .map_err(|e| format!("write {}: {e}", partial.display()))
            .and_then(|()| {
                // The rename replaces silently, so the check is what keeps an entry from
                // ever being overwritten: one host per state directory, and this runs under
                // the layer's write lock, so nothing can appear between the two.
                if path.exists() {
                    return Err(format!(
                        "{} exists: an entry is never overwritten",
                        path.display()
                    ));
                }
                std::fs::rename(&partial, &path)
                    .map_err(|e| format!("store {}: {e}", path.display()))
            });
        if let Err(e) = written {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
        self.index(meta.clone())?;
        Ok(meta)
    }

    /// Check what an entry may supersede: only an existing entry of THIS layer by the SAME
    /// author. `latest` answers "the newest entry about X that nothing supersedes", so a
    /// supersedes pointing at someone else's entry would let any writer retire it.
    pub fn check_supersedes(&self, draft: &Draft, author: &str) -> Result<(), Error> {
        for target in &draft.supersedes {
            let mine = target
                .strip_prefix(&entry_prefix(&self.layer))
                .and_then(|id| self.get(id))
                .map(|m| m.author == author);
            match mine {
                Some(true) => {}
                Some(false) => {
                    return Err(Error::Denied(format!(
                        "{target} is another participant's entry: an entry supersedes only \
                         its own author's"
                    )))
                }
                None => {
                    return Err(invalid(format!(
                        "ik:supersedes {target}, which is no entry of layer `{}`",
                        self.layer
                    )))
                }
            }
        }
        Ok(())
    }
}

fn read_dir(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        paths.push(
            entry
                .map_err(|e| format!("read {}: {e}", dir.display()))?
                .path(),
        );
    }
    paths.sort();
    Ok(paths)
}

/// An entry as its author sent it, checked for shape but not yet named or stamped.
#[derive(Debug)]
pub struct Draft {
    /// Its triples, every one about [`NEW_SUBJECT`].
    triples: Vec<Triple>,
    /// The entries it says it supersedes.
    pub supersedes: BTreeSet<String>,
}

/// Parse and check what an author sent to `say`: Turtle, or JSON-LD when it starts with
/// `{` or `[`.
///
/// Refused (as a bad `content`): more than the layer's `entry_max` bytes; JSON-LD nested
/// deeper than [`MAX_JSON_DEPTH`], or Turtle deeper than [`MAX_TURTLE_DEPTH`]; a parse error;
/// a named graph; anything but exactly one
/// subject, `<urn:layer:new>` or one blank node; a
/// blank node or a quoted triple anywhere else; any of the three stamps
/// (`prov:wasAttributedTo`, `dcterms:created`, `ik:seq`); no `rdf:type`, or a type no shape
/// of the layer targets (SHACL conforms vacuously for what nothing targets).
pub fn draft(def: &LayerDef, content: &[u8]) -> Result<Draft, Error> {
    let triples = parse_content(def, content)?;
    let new = node(NEW_SUBJECT);
    let mut blank = None;
    let mut placeholder = false;
    for t in &triples {
        match &t.subject {
            NamedOrBlankNode::NamedNode(n) if n.as_str() == NEW_SUBJECT => placeholder = true,
            NamedOrBlankNode::BlankNode(b) => match &blank {
                None => blank = Some(b.clone()),
                Some(one) if one == b => {}
                Some(_) => return Err(invalid("more than one subject: an entry is one subject")),
            },
            other => {
                return Err(invalid(format!(
                    "subject {other}: write about <{NEW_SUBJECT}> (or one blank node), and the \
                     host names the entry"
                )))
            }
        }
    }
    if placeholder && blank.is_some() {
        return Err(invalid("more than one subject: an entry is one subject"));
    }
    let supersedes_p = rdf::ik_supersedes();
    let mut checked = Vec::with_capacity(triples.len());
    let mut types = BTreeSet::new();
    let mut supersedes = BTreeSet::new();
    for t in triples {
        let p = t.predicate.as_str();
        refuse_stamp(p)?;
        let object = match t.object {
            Term::BlankNode(ref b) if Some(b) == blank.as_ref() => Term::NamedNode(new.clone()),
            Term::NamedNode(_) | Term::Literal(_) => t.object.clone(),
            _ => {
                return Err(invalid(
                    "a blank node or a quoted triple as an object: an entry is one subject, \
                     and every other node is named by an IRI",
                ))
            }
        };
        if p == rdf::RDF_TYPE {
            types.insert(admitted_type(def, &object)?);
        }
        if p == supersedes_p {
            match object_iri(&object) {
                Some(target) => {
                    supersedes.insert(target.to_string());
                }
                None => return Err(invalid("ik:supersedes names an entry by its IRI")),
            }
        }
        checked.push(Triple::new(new.clone(), t.predicate, object));
    }
    require_type(def, &types)?;
    Ok(Draft {
        triples: checked,
        supersedes,
    })
}

/// Parse what a writer sent: Turtle, or JSON-LD when it starts with `{` or `[`, at most the
/// layer's `entry_max` bytes (and at most [`MAX_JSON_DEPTH`] or [`MAX_TURTLE_DEPTH`] levels
/// deep, before anything is parsed), in the default graph only. What the triples may say is the
/// caller's (each model's) to check.
pub(crate) fn parse_content(def: &LayerDef, content: &[u8]) -> Result<Vec<Triple>, Error> {
    if content.len() > def.entry_max {
        return Err(invalid(format!(
            "{} bytes, over layer `{}`'s bound of {}",
            content.len(),
            def.name,
            def.entry_max
        )));
    }
    let first = content.iter().find(|b| !b.is_ascii_whitespace());
    let json = matches!(first, Some(b'{') | Some(b'['));
    if json {
        let depth = json_depth(content);
        if depth > MAX_JSON_DEPTH {
            return Err(invalid(format!(
                "JSON-LD nested {depth} levels deep, over the bound of {MAX_JSON_DEPTH}: an entry \
                 is one subject, and no entry needs more"
            )));
        }
    } else {
        let depth = turtle_depth(content);
        if depth > MAX_TURTLE_DEPTH {
            return Err(invalid(format!(
                "Turtle nested {depth} levels deep, over the bound of {MAX_TURTLE_DEPTH}: an \
                 entry is one subject, and no entry needs more"
            )));
        }
    }
    let format = if json {
        RdfFormat::from_media_type("application/ld+json").expect("a known format")
    } else {
        RdfFormat::Turtle
    };
    let mut triples = Vec::new();
    for quad in RdfParser::from_format(format).for_slice(content) {
        let quad = quad.map_err(|e| invalid(format!("not {}: {e}", format.name())))?;
        if !quad.graph_name.is_default_graph() {
            return Err(invalid("a named graph: an entry is one graph"));
        }
        triples.push(Triple::new(quad.subject, quad.predicate, quad.object));
    }
    if triples.is_empty() {
        return Err(invalid("an empty graph"));
    }
    Ok(triples)
}

/// The deepest JSON-LD a write may nest (objects and arrays together, the `@context`
/// included).
///
/// JSON-LD's parser recurses once per nested object, and a stack overflow ABORTS the
/// process, so depth is bounded BEFORE anything is parsed, whatever the size bound allows:
/// 1500 nested objects (12 KB, under the default 16 KiB `entry_max`) overflowed a 2 MiB
/// thread, the stack a door's runtime gives each worker (the audit of 2026-10-11). An entry
/// is one subject whose other nodes are IRIs, so real entries nest a handful of levels; 64
/// is the bound ikigai-store puts on SPARQL nesting, for the same reason.
pub const MAX_JSON_DEPTH: usize = 64;

/// The deepest nesting of objects and arrays in `json`, read byte by byte with no
/// recursion, outside strings (an escaped quote does not end one). Not a validator: the
/// parser judges the rest.
///
/// ```
/// use ikigai_layers::channel::json_depth;
///
/// assert_eq!(json_depth(b"{}"), 1);
/// assert_eq!(json_depth(br#"{"a": [{"b": "{[\"{["}]}"#), 3);
/// assert_eq!(json_depth(&b"[".repeat(100_000)), 100_000);
/// ```
pub fn json_depth(json: &[u8]) -> usize {
    let (mut depth, mut deepest) = (0usize, 0usize);
    let (mut in_string, mut escaped) = (false, false);
    for &b in json {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// The deepest Turtle a write may nest: triple terms `<<( … )>>`, reified triples `<< … >>`,
/// blank-node property lists `[ … ]`, collections `( … )` and annotations `{| … |}`, counted
/// together.
///
/// Turtle's parser keeps its own stack, but an RDF 1.2 triple term is a boxed tree, and the
/// parser clones it recursively when it emits the triple that holds it, so a stack overflow
/// ABORTS the process here too, whatever the size bound allows: triple terms nested 1,750 deep
/// (16 KB, under the default 16 KiB `entry_max`) overflowed a 2 MiB thread in a debug build,
/// and 4,500 deep (40 KB, under the largest `entry_max`) in a release build (ledger #2015).
/// Every host that links this crate parses them: its graph turns RDF 1.2 on. The other three
/// shapes did not overflow at 100,000 deep, and are bounded with them anyway, so the bound is
/// one rule about Turtle rather than one parser's current recursion. An entry is one subject
/// whose other nodes are IRIs, so real entries nest a handful of levels; 64 is the bound
/// [`MAX_JSON_DEPTH`] puts on JSON-LD, for the same reason.
pub const MAX_TURTLE_DEPTH: usize = 64;

/// The deepest nesting of Turtle's brackets in `turtle` (see [`MAX_TURTLE_DEPTH`]), read byte
/// by byte with no recursion, outside IRIs, strings and comments. Not a validator: the parser
/// judges the rest.
///
/// It reads those three as the parser's lexer does (oxttl's, which this crate parses with),
/// so it never skips a bracket the parser will see: an IRI runs from `<` (but `<<`) to the
/// first `>`, a string to its first unescaped closing quote (`"""` or `'''` for a long one),
/// a comment from `#` to the end of the line, and a backslash takes the next byte with it,
/// in a string, an IRI or a prefixed name (`:a\#b` is one name, not a comment). Where the two
/// could part (an invalid escape, a line break in a short string), the lexer reports an error,
/// and a write stops at its first error, so nothing past that point becomes a term.
///
/// ```
/// use ikigai_layers::channel::turtle_depth;
///
/// assert_eq!(turtle_depth(b"<urn:a> <urn:b> <urn:c> ."), 0);
/// assert_eq!(turtle_depth(b"<urn:a> <urn:b> [ <urn:c> ( 1 2 ) ] ."), 2);
/// assert_eq!(turtle_depth(b"<urn:a> <urn:b> <<( <urn:a> <urn:b> <urn:c> )>> ."), 1);
/// // Brackets in an IRI, a string, a long string or a comment are not nesting.
/// assert_eq!(turtle_depth(br#"<urn:a(> <urn:b> "[[(\"" , """ ") """ . # <<("#), 0);
/// // An escaped `#` in a prefixed name starts no comment.
/// assert_eq!(turtle_depth(br"<urn:a> :b\#c [ <urn:c> 1 ] ."), 1);
/// assert_eq!(turtle_depth(&b"<<(: a".repeat(100_000)), 100_000);
/// ```
pub fn turtle_depth(turtle: &[u8]) -> usize {
    let (mut depth, mut deepest) = (0usize, 0usize);
    let mut i = 0;
    while let Some(&b) = turtle.get(i) {
        let next = |k: usize| turtle.get(i + k).copied();
        match b {
            b'#' => {
                // A comment, to the end of the line.
                while turtle.get(i).is_some_and(|c| !matches!(c, b'\n' | b'\r')) {
                    i += 1;
                }
                continue;
            }
            b'\\' => i += 1, // an escape in a prefixed name: the next byte is part of it
            b'<' if next(1) == Some(b'<') => {
                // `<<(` (a triple term) or `<<` (a reified triple): one level either way.
                depth += 1;
                deepest = deepest.max(depth);
                i += if next(2) == Some(b'(') { 2 } else { 1 };
            }
            b'<' => {
                // An IRI, to the first `>`.
                i += 1;
                while let Some(&c) = turtle.get(i) {
                    match c {
                        b'>' => break,
                        b'\\' => i += 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                let long = next(1) == Some(b) && next(2) == Some(b);
                i += if long { 3 } else { 1 };
                while let Some(&c) = turtle.get(i) {
                    if c == b'\\' {
                        i += 2;
                        continue;
                    }
                    if c == b
                        && (!long
                            || (turtle.get(i + 1) == Some(&b) && turtle.get(i + 2) == Some(&b)))
                    {
                        i += if long { 2 } else { 0 };
                        break;
                    }
                    i += 1;
                }
            }
            b'[' | b'(' | b'{' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b')' if next(1) == Some(b'>') && next(2) == Some(b'>') => {
                // `)>>` closes a triple term: one level, as `<<(` opened it.
                depth = depth.saturating_sub(1);
                i += 2;
            }
            b'>' if next(1) == Some(b'>') => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b']' | b')' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    deepest
}

/// The predicates the host stamps (who, when, and the feed position), refused if sent.
pub(crate) fn refuse_stamp(predicate: &str) -> Result<(), Error> {
    if predicate == rdf::PROV_ATTRIBUTED
        || predicate == rdf::DCT_CREATED
        || predicate == rdf::ik_seq()
    {
        return Err(invalid(format!(
            "<{predicate}> is stamped by the host (who, when, and the feed position) and \
             refused if sent"
        )));
    }
    Ok(())
}

/// An `rdf:type` object, admitted only when a shape of the layer targets it: SHACL conforms
/// vacuously for what nothing targets.
pub(crate) fn admitted_type(def: &LayerDef, object: &Term) -> Result<String, Error> {
    match object_iri(object) {
        Some(class) if def.target_classes.contains(class) => Ok(class.to_string()),
        _ => Err(invalid(format!(
            "rdf:type {object}: layer `{}` admits only {}",
            def.name,
            list(&def.target_classes)
        ))),
    }
}

/// At least one admitted `rdf:type`.
pub(crate) fn require_type(def: &LayerDef, types: &BTreeSet<String>) -> Result<(), Error> {
    if types.is_empty() {
        return Err(invalid(format!(
            "no rdf:type: layer `{}` admits {}",
            def.name,
            list(&def.target_classes)
        )));
    }
    Ok(())
}

fn list(classes: &BTreeSet<String>) -> String {
    classes
        .iter()
        .map(|c| format!("<{c}>"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Draft {
    /// The entry as it will be stored: named `iri`, attributed to `author`, created at
    /// `created` (`xsd:dateTime`) and numbered `seq`.
    pub fn stamp(&self, iri: &str, author: &str, created: &str, seq: u64) -> Vec<Triple> {
        let subject = node(iri);
        let new = node(NEW_SUBJECT);
        let rename = |term: &Term| match term {
            Term::NamedNode(n) if *n == new => Term::NamedNode(subject.clone()),
            other => other.clone(),
        };
        let mut stamped: Vec<Triple> = self
            .triples
            .iter()
            .map(|t| Triple::new(subject.clone(), t.predicate.clone(), rename(&t.object)))
            .collect();
        stamped.push(Triple::new(
            subject.clone(),
            node(rdf::PROV_ATTRIBUTED),
            node(author),
        ));
        stamped.push(Triple::new(
            subject.clone(),
            node(rdf::DCT_CREATED),
            Literal::new_typed_literal(created, node(rdf::XSD_DATE_TIME)),
        ));
        stamped.push(Triple::new(
            subject,
            NamedNode::new(rdf::ik_seq()).expect("a constant IRI"),
            Literal::new_typed_literal(seq.to_string(), node(rdf::XSD_INTEGER)),
        ));
        stamped
    }
}
