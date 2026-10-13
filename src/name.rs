//! Names: layers, collaborations and participants, and the IRIs and capabilities built
//! from them.
//!
//! Every name here ends up in three places at once: an IRI (`urn:layer:{layer}`), a
//! capability token (`urn:cap:layer:{verb}:{layer}`) and, for a layer, an MCP tool name
//! (`layer-{layer}-{action}__{verb}`). So a name is checked once, here, for all three.

/// The longest layer name: MCP caps tool names at 64 characters, and the longest tool a
/// layer projects is `layer-{layer}-definition__source` (25 characters around the name).
pub const MAX_LAYER_NAME: usize = 24;

/// The prefix of every participant a collaboration names by a short name:
/// `urn:collab:participant:{slug}`.
pub const PARTICIPANT_PREFIX: &str = "urn:collab:participant:";

/// The prefix every host's door names an enrolled client by when nothing else names it:
/// `urn:iki:host:{name}:client:<fingerprint>` (ledger #1170, #1179). A machine participant
/// may be named this way, and then its fingerprint is read from its own name.
pub const HOST_CLIENT_PREFIX: &str = "urn:iki:host:";

/// The longest collaboration or participant short name.
pub const MAX_SHORT_NAME: usize = 48;

/// The subject an author writes for the entry the host is about to name: the host rewrites it
/// to the entry's own IRI when it admits the write. A participant writes it, the host reads it,
/// so it lives here, where both sides link it (also `channel::NEW_SUBJECT`).
pub const NEW_SUBJECT: &str = "urn:layer:new";

/// A word in the `[a-z0-9-]` alphabet, starting with a letter or digit and not ending with a
/// hyphen, and no Windows device name ([`is_windows_device`]). The ledger's rule for names,
/// used for layers, collaborations and participants; a layer's name is joined to the state
/// directory and a participant's to its layers' `graphs/`, so a name is also a path segment
/// on every platform (ledger #1307).
fn is_word(name: &str, max: usize) -> bool {
    !name.is_empty()
        && name.len() <= max
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !is_windows_device(name)
}

/// Whether Windows reads `name` as a device rather than a file: `con`, `prn`, `aux`, `nul`,
/// `com0`..`com9`, `lpt0`..`lpt9` (and their superscript-digit spellings), `conin$` and
/// `conout$`, in any case, with or without an extension (Windows matches the part before the
/// first dot, ignoring trailing spaces). On Windows `<state>\nul` is the null device, never a
/// directory (ledger #1307).
///
/// ```
/// use ikigai_layers::name::is_windows_device;
///
/// assert!(is_windows_device("nul"));
/// assert!(is_windows_device("COM1"));
/// assert!(is_windows_device("aux.ttl"));
/// assert!(!is_windows_device("nullable"));
/// assert!(!is_windows_device("com10"));
/// ```
pub fn is_windows_device(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let lower = stem.to_lowercase();
    if matches!(
        lower.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) {
        return true;
    }
    let mut chars = lower.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (prefix == "com" || prefix == "lpt")
        && rest.len() == 1
        && (rest[0].is_ascii_digit() || matches!(rest[0], '\u{b9}' | '\u{b2}' | '\u{b3}'))
}

/// Whether `name` is one plain file name, safe to join to a directory on every platform:
/// not empty, not starting with a dot (so never `.` or `..`), no `/`, `\`, `:` (a Windows
/// drive prefix, `C:x`, makes `Path::join` REPLACE the base), no character Windows refuses in a
/// file name (`<>"|?*` and control characters), not ending in a dot or a space (Windows drops
/// them, so the name would read another file), and no Windows device name (ledger #1307).
///
/// ```
/// use ikigai_layers::name::is_plain_file_name;
///
/// assert!(is_plain_file_name("talk.shapes.ttl"));
/// assert!(!is_plain_file_name("C:talk.shapes.ttl"));
/// assert!(!is_plain_file_name("../talk.shapes.ttl"));
/// assert!(!is_plain_file_name("con.ttl"));
/// assert!(!is_plain_file_name("talk.ttl."));
/// ```
pub fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && !name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
        && !is_windows_device(name)
}

/// Whether `name` is a usable layer name: lowercase `[a-z0-9-]`, at most
/// [`MAX_LAYER_NAME`] characters, no leading or trailing hyphen, and no Windows device name
/// ([`is_windows_device`]).
///
/// ```
/// use ikigai_layers::name::is_layer_name;
///
/// assert!(is_layer_name("talk"));
/// assert!(is_layer_name("tasks-bobby"));
/// assert!(is_layer_name("abcdefghijklmnopqrstuvwx")); // 24
/// assert!(!is_layer_name("abcdefghijklmnopqrstuvwxy")); // 25
/// assert!(!is_layer_name("Talk"));
/// assert!(!is_layer_name("talk:x"));
/// assert!(!is_layer_name("-talk"));
/// assert!(!is_layer_name("nul"));
/// ```
pub fn is_layer_name(name: &str) -> bool {
    is_word(name, MAX_LAYER_NAME)
}

/// Whether `name` is a usable collaboration or participant short name: the same alphabet
/// as a layer, at most [`MAX_SHORT_NAME`] characters.
pub fn is_short_name(name: &str) -> bool {
    is_word(name, MAX_SHORT_NAME)
}

/// `urn:layer:{layer}`.
pub fn layer_iri(layer: &str) -> String {
    format!("urn:layer:{layer}")
}

/// `urn:collab:{name}`.
pub fn collab_iri(name: &str) -> String {
    format!("urn:collab:{name}")
}

/// The exact capability a verb on a layer requires: `urn:cap:layer:{verb}:{layer}`. The
/// parameter goes LAST (core has no infix wildcard), as in the ledger's
/// `urn:cap:ledger:{verb}:{name}`.
///
/// ```
/// use ikigai_layers::name::layer_cap;
///
/// assert_eq!(layer_cap("say", "talk"), "urn:cap:layer:say:talk");
/// ```
pub fn layer_cap(verb: &str, layer: &str) -> String {
    format!("urn:cap:layer:{verb}:{layer}")
}

/// The capability that reads a collaboration's manifest: `urn:cap:collab:read:{name}`.
pub fn collab_cap(name: &str) -> String {
    format!("urn:cap:collab:read:{name}")
}

/// The endpoint id of one layer action: `layer-{layer}-{action}`. Its MCP tool is this
/// id, two underscores and the verb.
pub fn endpoint_id(layer: &str, action: &str) -> String {
    format!("layer-{layer}-{action}")
}

/// A participant, by the IRI its door names it with.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Participant {
    iri: String,
    slug: String,
    /// The fingerprint read from a `urn:iki:host:{name}:client:<fp>` name, when it is one.
    fingerprint: Option<String>,
}

impl Participant {
    /// Parse a participant IRI. Two forms are accepted, and nothing else, because each
    /// gives the participant a name that is safe in a path, an IRI segment and a grant:
    ///
    /// - `urn:collab:participant:{slug}`, `{slug}` a short name, whose author graph is
    ///   `graph:{slug}`;
    /// - `urn:iki:host:{host}:client:<fingerprint>`, a machine named the way its door
    ///   names it (64 lowercase hex digits), whose author graph is
    ///   `graph:client.{host}.{fingerprint}` (the dots keep it apart from every short
    ///   name).
    ///
    /// ```
    /// use ikigai_layers::name::Participant;
    ///
    /// let bobby = Participant::parse("urn:collab:participant:bobby").unwrap();
    /// assert_eq!(bobby.slug(), "bobby");
    /// assert_eq!(bobby.fingerprint(), None);
    ///
    /// let fp = "6f1c00000000000000000000000000000000000000000000000000000000abcd";
    /// let gonk = Participant::parse(&format!("urn:iki:host:layers:client:{fp}")).unwrap();
    /// assert_eq!(gonk.slug(), format!("client.layers.{fp}"));
    /// assert_eq!(gonk.fingerprint(), Some(fp));
    ///
    /// assert!(Participant::parse("urn:example:alice").is_err());
    /// assert!(Participant::parse("urn:collab:participant:Bob").is_err());
    /// ```
    pub fn parse(iri: &str) -> Result<Participant, String> {
        if let Some(slug) = iri.strip_prefix(PARTICIPANT_PREFIX) {
            if !is_short_name(slug) {
                return Err(format!(
                    "`{iri}`: a participant's short name is lowercase [a-z0-9-], at most \
                     {MAX_SHORT_NAME} characters, and no Windows device name"
                ));
            }
            return Ok(Participant {
                iri: iri.to_string(),
                slug: slug.to_string(),
                fingerprint: None,
            });
        }
        if let Some(rest) = iri.strip_prefix(HOST_CLIENT_PREFIX) {
            if let Some((host, fp)) = rest.split_once(":client:") {
                let hex = fp.len() == 64
                    && fp
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                if is_short_name(host) && hex {
                    return Ok(Participant {
                        iri: iri.to_string(),
                        slug: format!("client.{host}.{fp}"),
                        fingerprint: Some(fp.to_string()),
                    });
                }
            }
            return Err(format!(
                "`{iri}`: a host client is urn:iki:host:{{host}}:client:<64 lowercase hex>"
            ));
        }
        Err(format!(
            "`{iri}`: a participant is {PARTICIPANT_PREFIX}{{name}} or \
             {HOST_CLIENT_PREFIX}{{host}}:client:<fingerprint>"
        ))
    }

    /// Resolve how a manifest names a participant: a short name means
    /// `urn:collab:participant:{name}`; anything with a colon is a full IRI.
    pub fn from_manifest_key(key: &str) -> Result<Participant, String> {
        if key.contains(':') {
            Participant::parse(key)
        } else {
            Participant::parse(&format!("{PARTICIPANT_PREFIX}{key}"))
        }
    }

    /// The IRI.
    pub fn iri(&self) -> &str {
        &self.iri
    }

    /// The name of this participant's author graph and its grant.
    pub fn slug(&self) -> &str {
        &self.slug
    }

    /// The certificate fingerprint its own name carries, for a host-client participant.
    pub fn fingerprint(&self) -> Option<&str> {
        self.fingerprint.as_deref()
    }
}

/// Fold a certificate fingerprint the way the door does: colons and whitespace dropped,
/// lowercase. `None` unless 64 hex digits remain.
///
/// ```
/// use ikigai_layers::name::fold_fingerprint;
///
/// let colons = "6F:1C".to_string() + &":00".repeat(29) + ":AB";
/// assert_eq!(fold_fingerprint(&colons).unwrap().len(), 64);
/// assert!(fold_fingerprint("not-a-fingerprint").is_none());
/// ```
pub fn fold_fingerprint(fp: &str) -> Option<String> {
    let folded: String = fp
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (folded.len() == 64 && folded.bytes().all(|b| b.is_ascii_hexdigit())).then_some(folded)
}
