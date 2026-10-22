//! Onboarding a node by itself (ledger #2009): an enrollment LOBBY a certificate nobody
//! enrolled may say one request on, and signed ENTITLEMENTS the host applies to its own
//! door's enrollment.
//!
//! **The two layers are configuration**, like every layer (the example pair is in
//! `onboard/` of this repository):
//!
//! - a **lobby**, a channel marked `lobby = true`, whose closed shapes admit one entry type,
//!   [`ENROLLMENT_REQUEST`]. A door admits an unenrolled certificate to `say` on it and
//!   nothing else ([`lobby_capability`]); the host holds every lobby to
//!   [`LOBBY_ENTRY_MAX`] bytes an entry and [`LOBBY_SAYS_PER_FINGERPRINT`] /
//!   [`LOBBY_SAYS_TOTAL`] says an hour, and checks a request's certificate against its
//!   poster ([`check_request`]);
//! - a **grants** layer, a channel only the controller says on, whose entries are
//!   [`ENTITLEMENT`]s signed with ikigai-sign under the controller's key. The host verifies
//!   each new one against the key in its configuration (`[onboard]` in `layers.toml`,
//!   [`crate::config::OnboardConfig`]) and applies what verifies to the files its door reads:
//!   `participants.ttl`, `entitlements.toml` and through the grants generator `grants.json`
//!   and `clients.json`, and the certificate into the door's trust directory. What it saw,
//!   applied and refused, and why, is [`STATUS`].
//!
//! **What a request is**, as the poster writes it (one subject, the host names it):
//!
//! ```turtle
//! @prefix onb: <https://ikigai-rs.dev/ns/onboard#> .
//! <urn:iki:onboard:request:{fingerprint}> a onb:EnrollmentRequest ;
//!     onb:invite <urn:iki:onboard:invite:{id}> ;
//!     onb:certificate "-----BEGIN CERTIFICATE-----…" ;
//!     onb:fingerprint "{lowercase hex SHA-256 of the leaf DER}" ;
//!     onb:proof "{lowercase hex HMAC-SHA256(key = the pairing code, message = the fingerprint)}" .
//! ```
//!
//! A lobby takes the subject `<urn:iki:onboard:request:{fingerprint}>` (or
//! `<urn:layer:new>`) and names the entry itself, `urn:layer:{lobby}:entry:{id}`. The host
//! checks the certificate and the fingerprint (and the one in the subject); the PROOF is the
//! controller's to check, since only the controller knows the pairing code.
//!
//! **What an entitlement is**, as the controller writes it:
//!
//! ```turtle
//! @prefix onb: <https://ikigai-rs.dev/ns/onboard#> .
//! @prefix ik: <https://ikigai-rs.dev/ns#> .
//! <urn:layer:new> a onb:Entitlement ;
//!     onb:fingerprint "{hex}" ;
//!     onb:principal <urn:collab:participant:{name}> ;
//!     onb:kind <urn:iki:onboard:kind:{kind}> ;
//!     onb:request <urn:layer:enroll:entry:{id}> ;        # the request it grants
//!     onb:layerGrant <urn:cap:layer:read:tasks> , <urn:cap:layer:out:tasks> ;
//!     onb:gonkGrant "default=worker" ;                   # gonk's half; this host ignores it
//!     onb:router "edge" ;                                # gonk's half too
//!     onb:expires "2026-11-22T00:00:00Z"^^xsd:dateTime ;
//!     ik:supersedes <urn:layer:grants:entry:{id}> ;      # the earlier one, if any
//!     onb:signature "{the signature graph, Turtle}" .
//! ```
//!
//! Every `onb:layerGrant` is a capability IRI, `urn:cap:layer:{verb}:{layer}`, never a
//! nested node: a channel entry is ONE subject. Applied, an entitlement also grants `read` on
//! the grants layer, so the node reads its own entitlement there with the certificate it
//! names. Only `onboard.controller` may say on the grants layer.
//!
//! `onb:signature` is ikigai-sign's signature graph (`urn:sign:sign`) over
//! [`signing_bytes`] of everything else: the controller signs the entry as it will say it,
//! without the signature, and the host verifies the entry as it stored it, without the
//! signature and without its own three stamps. A revocation is an entitlement with no
//! `onb:layerGrant` that supersedes the current one.

use ikigai_core::Capability;
use oxrdf::{NamedOrBlankNode, Term, Triple};

use crate::name::{fold_fingerprint, is_layer_name, is_short_name, layer_cap, HOST_CLIENT_PREFIX};
use crate::rdf::{self, node};

#[cfg(feature = "host")]
pub(crate) mod apply;

#[cfg(feature = "host")]
pub use apply::{Effective, Record, Status, Verdict};

/// The onboarding vocabulary, `onb:`. New with ledger #2009, and not yet in the published
/// `/ns` vocabulary: its terms are the ones below.
pub const ONB: &str = "https://ikigai-rs.dev/ns/onboard#";

/// `onb:EnrollmentRequest`: what a node says on a lobby.
pub const ENROLLMENT_REQUEST: &str = "https://ikigai-rs.dev/ns/onboard#EnrollmentRequest";
/// `onb:Entitlement`: what the controller says on the grants layer.
pub const ENTITLEMENT: &str = "https://ikigai-rs.dev/ns/onboard#Entitlement";
/// `onb:invite`: the invitation a request answers.
pub const INVITE: &str = "https://ikigai-rs.dev/ns/onboard#invite";
/// `onb:certificate`: the requester's certificate, PEM.
pub const CERTIFICATE: &str = "https://ikigai-rs.dev/ns/onboard#certificate";
/// `onb:fingerprint`: lowercase hex SHA-256 of a certificate's leaf DER.
pub const FINGERPRINT: &str = "https://ikigai-rs.dev/ns/onboard#fingerprint";
/// `onb:proof`: HMAC-SHA256 of the fingerprint under the pairing code, lowercase hex.
pub const PROOF: &str = "https://ikigai-rs.dev/ns/onboard#proof";
/// `onb:principal`: the participant an entitlement makes the certificate.
pub const PRINCIPAL: &str = "https://ikigai-rs.dev/ns/onboard#principal";
/// `onb:kind`: the node kind an entitlement was expanded from.
pub const KIND: &str = "https://ikigai-rs.dev/ns/onboard#kind";
/// `onb:request`: the lobby entry an entitlement grants (where its certificate comes from).
pub const REQUEST: &str = "https://ikigai-rs.dev/ns/onboard#request";
/// `onb:layerGrant`: one layer capability an entitlement grants, `urn:cap:layer:{verb}:{layer}`.
pub const LAYER_GRANT: &str = "https://ikigai-rs.dev/ns/onboard#layerGrant";
/// `onb:gonkGrant`: gonk's half of an entitlement (`{ledger}={role}`); this host ignores it.
pub const GONK_GRANT: &str = "https://ikigai-rs.dev/ns/onboard#gonkGrant";
/// `onb:router`: a worker's router labels (gonk's half; this host ignores it).
pub const ROUTER: &str = "https://ikigai-rs.dev/ns/onboard#router";
/// `onb:gonkDoor`: gonk's door, `HOST:PORT`, carried from the kind (gonk's half).
pub const GONK_DOOR: &str = "https://ikigai-rs.dev/ns/onboard#gonkDoor";
/// `onb:gonkServerCert`: gonk's server certificate, PEM, carried from the kind (gonk's half).
pub const GONK_SERVER_CERT: &str = "https://ikigai-rs.dev/ns/onboard#gonkServerCert";
/// `onb:expires`: when an entitlement stops granting.
pub const EXPIRES: &str = "https://ikigai-rs.dev/ns/onboard#expires";
/// `onb:signature`: the ikigai-sign signature graph over [`signing_bytes`], as Turtle.
pub const SIGNATURE: &str = "https://ikigai-rs.dev/ns/onboard#signature";

/// The host's onboarding status: what it saw on its grants layer, what it applied, what it
/// refused and why, and whether its door must be restarted. Readable by whoever may say on
/// the grants layer (the controller): `urn:cap:layer:say:{grants}`.
pub const STATUS: &str = "urn:layer:onboard:status";

/// The bound on one lobby entry: 16 KiB, the default `entry_max`, which a lobby may lower and
/// never raise.
pub const LOBBY_ENTRY_MAX: usize = 16 * 1024;
/// The most `say`s one certificate may make on a lobby in [`LOBBY_WINDOW_MILLIS`].
pub const LOBBY_SAYS_PER_FINGERPRINT: usize = 5;
/// The most `say`s a lobby takes from everyone together in [`LOBBY_WINDOW_MILLIS`].
pub const LOBBY_SAYS_TOTAL: usize = 30;
/// The window both lobby rates count over: one hour, by the kernel clock.
pub const LOBBY_WINDOW_MILLIS: u64 = 60 * 60 * 1000;

/// The principal a door mints for a certificate it admits to a lobby only:
/// `urn:iki:host:{host}:client:{fingerprint}`, the name a door gives any client nothing else
/// names, so the fingerprint the host checks a request against is in the name itself.
///
/// ```
/// use ikigai_layers::onboard::lobby_principal;
///
/// let fp = "AB".repeat(32);
/// assert_eq!(
///     lobby_principal("edge", &fp).unwrap(),
///     format!("urn:iki:host:edge:client:{}", "ab".repeat(32))
/// );
/// assert!(lobby_principal("edge", "not-a-fingerprint").is_err());
/// ```
pub fn lobby_principal(host: &str, fingerprint: &str) -> Result<String, String> {
    let fp = fold_fingerprint(fingerprint)
        .ok_or_else(|| format!("`{fingerprint}` is not a SHA-256 fingerprint (64 hex digits)"))?;
    if !is_short_name(host) {
        return Err(format!(
            "`{host}` is not a host name (lowercase [a-z0-9-], at most {} characters)",
            crate::name::MAX_SHORT_NAME
        ));
    }
    Ok(format!("{HOST_CLIENT_PREFIX}{host}:client:{fp}"))
}

/// The capability a door mints for a certificate it did NOT enroll, on a host with a lobby
/// `layer`: `urn:cap:layer:say:{layer}` and nothing else, under [`lobby_principal`]. So the
/// manifold such a certificate sees is the lobby's `say`, and every other verb and layer is
/// `Denied`.
///
/// ```
/// use ikigai_core::Capability;
/// use ikigai_layers::onboard::lobby_capability;
///
/// let fp = "ab".repeat(32);
/// let principal = format!("urn:iki:host:edge:client:{fp}");
/// let cap = lobby_capability("enroll", "edge", &fp).unwrap();
/// assert_eq!(
///     cap,
///     Capability::scoped(["urn:cap:layer:say:enroll"]).with_principal(&principal).unwrap()
/// );
/// assert_eq!(cap.principal(), Some(principal.as_str()));
/// ```
pub fn lobby_capability(layer: &str, host: &str, fingerprint: &str) -> Result<Capability, String> {
    if !is_layer_name(layer) {
        return Err(format!("`{layer}` is not a layer name"));
    }
    let principal = lobby_principal(host, fingerprint)?;
    Capability::scoped([layer_cap("say", layer)])
        .with_principal(&principal)
        .map_err(|e| format!("{principal}: {e}"))
}

/// The bytes an entitlement's signature covers: every triple about `subject` but its
/// [`SIGNATURE`] and the host's three stamps (`prov:wasAttributedTo`, `dcterms:created`,
/// `ik:seq`), with `subject` written as `<urn:layer:new>` wherever it appears, each triple
/// as one N-Triples line, the lines sorted bytewise with duplicates dropped, each ending in
/// a line feed.
///
/// So the controller signs exactly what it will say (about `<urn:layer:new>`, before the host
/// names and stamps it), and the host verifies exactly what it stored. Triples about any
/// other subject are not covered (a stored entry has none).
///
/// ```
/// use ikigai_layers::onboard::signing_bytes_of;
///
/// let said = r#"@prefix onb: <https://ikigai-rs.dev/ns/onboard#> .
/// <urn:layer:new> a onb:Entitlement ; onb:fingerprint "ab" ; onb:signature "x" ."#;
/// assert_eq!(
///     String::from_utf8(signing_bytes_of(said, "urn:layer:new").unwrap()).unwrap(),
///     [
///         "<urn:layer:new> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
///          <https://ikigai-rs.dev/ns/onboard#Entitlement> .\n",
///         "<urn:layer:new> <https://ikigai-rs.dev/ns/onboard#fingerprint> \"ab\" .\n",
///     ]
///     .concat()
/// );
/// ```
pub fn signing_bytes(triples: &[Triple], subject: &str) -> Vec<u8> {
    let new = node(crate::name::NEW_SUBJECT);
    let seq = rdf::ik_seq();
    let mut lines: Vec<String> = triples
        .iter()
        .filter(|t| rdf::subject_iri(&t.subject) == Some(subject))
        .filter(|t| {
            let p = t.predicate.as_str();
            p != SIGNATURE && p != rdf::PROV_ATTRIBUTED && p != rdf::DCT_CREATED && p != seq
        })
        .map(|t| {
            let object = match &t.object {
                Term::NamedNode(n) if n.as_str() == subject => Term::NamedNode(new.clone()),
                other => other.clone(),
            };
            let renamed = Triple::new(
                NamedOrBlankNode::NamedNode(new.clone()),
                t.predicate.clone(),
                object,
            );
            let mut line = oxttl::NTriplesSerializer::new().for_writer(Vec::new());
            line.serialize_triple(&renamed)
                .expect("writing N-Triples to memory cannot fail");
            String::from_utf8(line.finish()).expect("N-Triples is UTF-8")
        })
        .collect();
    lines.sort();
    lines.dedup();
    lines.concat().into_bytes()
}

/// [`signing_bytes`] of a Turtle document: what a controller calls on the entitlement it is
/// about to sign (`subject` = `urn:layer:new`).
pub fn signing_bytes_of(turtle: &str, subject: &str) -> Result<Vec<u8>, String> {
    Ok(signing_bytes(&rdf::parse_turtle(turtle)?, subject))
}

/// The lowercase hex SHA-256 of the leaf DER of `pem`'s one certificate: the fingerprint a
/// door names the certificate by. Refused: anything but exactly one `CERTIFICATE` block
/// (RFC 7468, strict), or DER that is not one well-formed DER document.
///
/// ```
/// use ikigai_layers::onboard::fingerprint_of_pem;
///
/// // The smallest well-formed DER document: an empty SEQUENCE.
/// let pem = "-----BEGIN CERTIFICATE-----\nMAA=\n-----END CERTIFICATE-----\n";
/// assert_eq!(
///     fingerprint_of_pem(pem).unwrap(),
///     "e4f60d0aa6d7f3d3b6a6494b1c861b99f649c6f9ec51abaf201b20f297327c95"
/// );
/// assert!(fingerprint_of_pem("-----BEGIN PUBLIC KEY-----\nMAA=\n-----END PUBLIC KEY-----\n").is_err());
/// ```
#[cfg(feature = "host")]
pub fn fingerprint_of_pem(pem: &str) -> Result<String, String> {
    use sha2::Digest;
    let (label, der) = pem_rfc7468::decode_vec(pem.trim().as_bytes())
        .map_err(|e| format!("not one PEM certificate: {e}"))?;
    if label != "CERTIFICATE" {
        return Err(format!("a PEM `{label}`, not a CERTIFICATE"));
    }
    <der::Document as der::Decode>::from_der(&der)
        .map_err(|e| format!("the certificate is not DER: {e}"))?;
    Ok(sha2::Sha256::digest(&der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Check that `pem` is one SPKI `PUBLIC KEY` block (RFC 7468, strict) of well-formed DER: the
/// shape `onboard.controller_key` must have. Whether it is an Ed25519 key is ikigai-sign's to
/// say, at the first verification.
#[cfg(feature = "host")]
pub fn check_public_key_pem(pem: &str) -> Result<(), String> {
    let (label, der) = pem_rfc7468::decode_vec(pem.trim().as_bytes())
        .map_err(|e| format!("not one PEM public key: {e}"))?;
    if label != "PUBLIC KEY" {
        return Err(format!("a PEM `{label}`, not a PUBLIC KEY (SPKI)"));
    }
    <der::Document as der::Decode>::from_der(&der)
        .map_err(|e| format!("the key is not DER: {e}"))?;
    Ok(())
}

/// The one literal value of `predicate` in `triples`, if there is exactly one.
#[cfg(feature = "host")]
pub(crate) fn one_literal<'a>(triples: &'a [Triple], predicate: &str) -> Option<&'a str> {
    let mut values = triples.iter().filter(|t| t.predicate.as_str() == predicate);
    match (values.next(), values.next()) {
        (Some(t), None) => match &t.object {
            Term::Literal(l) => Some(l.value()),
            _ => None,
        },
        _ => None,
    }
}

/// The prefix of the subject the onboarding contract names a request by:
/// `urn:iki:onboard:request:{fingerprint}`. A lobby takes it as the entry's placeholder, as it
/// takes `<urn:layer:new>`, and names the entry itself.
pub const REQUEST_SUBJECT_PREFIX: &str = "urn:iki:onboard:request:";

/// The fingerprint in a request subject, `urn:iki:onboard:request:{64 lowercase hex}`.
///
/// ```
/// use ikigai_layers::onboard::request_subject_fingerprint;
///
/// let fp = "ab".repeat(32);
/// assert_eq!(
///     request_subject_fingerprint(&format!("urn:iki:onboard:request:{fp}")),
///     Some(fp.as_str())
/// );
/// assert_eq!(request_subject_fingerprint("urn:iki:onboard:request:AB"), None);
/// ```
pub fn request_subject_fingerprint(iri: &str) -> Option<&str> {
    let fp = iri.strip_prefix(REQUEST_SUBJECT_PREFIX)?;
    (fp.len() == 64
        && fp
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(fp)
}

/// The check a host makes on a [`ENROLLMENT_REQUEST`] before it admits it, beyond its shapes:
/// the poster's certificate fingerprint (read from its principal, [`lobby_principal`]'s form)
/// must equal `onb:fingerprint`, and `onb:fingerprint` must be the SHA-256 of the posted
/// `onb:certificate`'s leaf DER, recomputed here and never trusted from the entry.
///
/// `posted` is the subject the author wrote, when it was the contract's
/// `urn:iki:onboard:request:{fingerprint}`: that fingerprint must be the stated one too.
///
/// A poster whose name carries no fingerprint (an enrolled participant) is `Denied`: the host
/// cannot tie the certificate to the connection that posted it. A certificate that does not
/// hash to its fingerprint is a bad `content`.
#[cfg(feature = "host")]
pub fn check_request(
    triples: &[Triple],
    posted: Option<&str>,
    author: &str,
) -> Result<(), ikigai_core::Error> {
    use crate::channel::invalid;
    let poster = crate::name::Participant::parse(author)
        .ok()
        .and_then(|p| p.fingerprint().map(str::to_string))
        .ok_or_else(|| {
            ikigai_core::Error::Denied(format!(
                "an enrollment request is said by the certificate it enrolls, named by its \
                 fingerprint ({HOST_CLIENT_PREFIX}{{host}}:client:<fingerprint>); {author} \
                 carries none"
            ))
        })?;
    let stated = one_literal(triples, FINGERPRINT)
        .ok_or_else(|| invalid("an enrollment request states exactly one onb:fingerprint"))?;
    if let Some(named) = posted.and_then(request_subject_fingerprint) {
        if named != stated {
            return Err(invalid(format!(
                "the request is named for {named} and states onb:fingerprint {stated}"
            )));
        }
    }
    if stated != poster {
        return Err(ikigai_core::Error::Denied(format!(
            "onb:fingerprint {stated} is not the certificate that said it ({poster}): a node \
             requests enrollment for its own certificate only"
        )));
    }
    let pem = one_literal(triples, CERTIFICATE)
        .ok_or_else(|| invalid("an enrollment request carries exactly one onb:certificate"))?;
    let actual = fingerprint_of_pem(pem).map_err(|e| invalid(format!("onb:certificate: {e}")))?;
    if actual != stated {
        return Err(invalid(format!(
            "onb:certificate hashes to {actual}, not to its onb:fingerprint {stated}"
        )));
    }
    Ok(())
}
