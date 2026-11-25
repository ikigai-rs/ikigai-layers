//! The configuration resources, read-only (ledger #4307; `docs/design/runtime-layers.md`
//! section 2, follow-up 1): every file the host reads, served as one keyed resource under
//! `urn:layers:`, and the compositions derived from them. Every one declares
//! [`CAP_ADMIN_READ`], which no manifest grants (only the `[admin]` table of `layers.toml`
//! does, [`crate::config::load_admins`]), and none reads a layer's entries.
//!
//! ```text
//! urn:layers:layer                                       every layer: model, kind, who mounts it
//! urn:layers:layer:name:{layer}                          layers/{layer}.toml, as written
//! urn:layers:shape | context | scheme                    every document a definition names
//! urn:layers:shape:name:{name} (context, scheme alike)   the document, as written
//! urn:layers:collab                                      every collaboration and its mounts
//! urn:layers:collab:name:{collab}                        collabs/{collab}.toml, as written
//! urn:layers:collab:name:{collab}:mount:layer:{layer}    one [[mount]] table of it
//! urn:layers:participant                                 the participants graph
//! urn:layers:participant:name:{name}                     one participant's statements (endpoints.rs)
//! urn:layers:enrollment                                  every enrolled fingerprint
//! urn:layers:enrollment:fingerprint:{fingerprint}        one of them, and whom it names
//! urn:layers:grant | client                              grants.json, clients.json, derived
//! urn:layers:revision                                    a digest over the files read
//! ```
//!
//! Each reads the files as they are now, on every request, so it answers what is on disk
//! (the revision included), not what the host read when it opened. A whole-file atom answers
//! the file's bytes unchanged; a set that needs the configuration's meaning (a layer's model,
//! a mount's layer) reads and checks it as the generator does ([`Config::load`]), so a
//! configuration that would not open answers the same refusal the generator prints.

use super::*;

use crate::config::{documents, revision, Config, Document, DocumentKind, CAP_ADMIN_READ};
use crate::grants::{generate, load_enrolled, load_fingerprints};
use crate::name::{fold_fingerprint, is_layer_name, PARTICIPANT_PREFIX};

const TOML: &str = "application/toml";

/// `urn:layers:layer`.
pub const LAYERS: &str = "urn:layers:layer";
/// `urn:layers:collab`.
pub const COLLABS: &str = "urn:layers:collab";
/// `urn:layers:participant`.
pub const PARTICIPANTS: &str = "urn:layers:participant";
/// `urn:layers:enrollment`.
pub const ENROLLMENTS: &str = "urn:layers:enrollment";
/// `urn:layers:grant`.
pub const GRANTS: &str = "urn:layers:grant";
/// `urn:layers:client`.
pub const CLIENTS: &str = "urn:layers:client";
/// `urn:layers:revision`.
pub const REVISION: &str = "urn:layers:revision";

/// The atom naming layer `layer`'s definition.
pub fn layer_resource(layer: &str) -> String {
    format!("{LAYERS}:name:{layer}")
}

/// The set of documents of `kind`: `urn:layers:shape`, `…:context` or `…:scheme`.
pub fn documents_resource(kind: DocumentKind) -> String {
    format!("urn:layers:{}", type_of(kind))
}

/// The atom naming one document.
pub fn document_resource(kind: DocumentKind, name: &str) -> String {
    format!("{}:name:{name}", documents_resource(kind))
}

/// The atom naming collaboration `collab`'s manifest.
pub fn collab_resource(collab: &str) -> String {
    format!("{COLLABS}:name:{collab}")
}

/// The atom naming one mount of a manifest.
pub fn mount_resource(collab: &str, layer: &str) -> String {
    format!("{}:mount:layer:{layer}", collab_resource(collab))
}

/// The atom naming participant `name` (a short name).
pub fn participant_resource(name: &str) -> String {
    format!("{PARTICIPANTS}:name:{name}")
}

/// The atom naming one enrolled certificate.
pub fn enrollment_resource(fingerprint: &str) -> String {
    format!("{ENROLLMENTS}:fingerprint:{fingerprint}")
}

fn type_of(kind: DocumentKind) -> &'static str {
    match kind {
        DocumentKind::Shape => "shape",
        DocumentKind::Context => "context",
        DocumentKind::Scheme => "scheme",
    }
}

fn media_of(kind: DocumentKind) -> &'static str {
    match kind {
        DocumentKind::Context => JSON_LD,
        DocumentKind::Shape | DocumentKind::Scheme => TURTLE,
    }
}

/// Which configuration resource one endpoint serves.
#[derive(Clone, Copy, Debug)]
pub(super) enum Admin {
    Layers,
    Layer,
    Documents(DocumentKind),
    Document(DocumentKind),
    Collabs,
    Mount,
    Collab,
    Participants,
    Enrollments,
    Enrollment,
    Grants,
    Clients,
    Revision,
}

impl Admin {
    /// Every resource, in the order the space binds them: a mount before its collaboration,
    /// since the first binding that matches wins and a final `{collab}` would capture the rest.
    pub(super) fn all() -> Vec<Admin> {
        let mut all = vec![Admin::Layers, Admin::Layer];
        for kind in DocumentKind::ALL {
            all.push(Admin::Documents(kind));
            all.push(Admin::Document(kind));
        }
        all.extend([
            Admin::Collabs,
            Admin::Mount,
            Admin::Collab,
            Admin::Participants,
            Admin::Enrollments,
            Admin::Enrollment,
            Admin::Grants,
            Admin::Clients,
            Admin::Revision,
        ]);
        all
    }

    /// The IRI it is bound at, a template when it is keyed.
    pub(super) fn iri(self) -> String {
        match self {
            Admin::Layers => LAYERS.into(),
            Admin::Layer => layer_resource("{layer}"),
            Admin::Documents(kind) => documents_resource(kind),
            Admin::Document(kind) => document_resource(kind, "{name}"),
            Admin::Collabs => COLLABS.into(),
            Admin::Mount => mount_resource("{collab}", "{layer}"),
            Admin::Collab => collab_resource("{collab}"),
            Admin::Participants => PARTICIPANTS.into(),
            Admin::Enrollments => ENROLLMENTS.into(),
            Admin::Enrollment => enrollment_resource("{fingerprint}"),
            Admin::Grants => GRANTS.into(),
            Admin::Clients => CLIENTS.into(),
            Admin::Revision => REVISION.into(),
        }
    }

    /// The keys its IRI binds.
    fn keys(self) -> &'static [&'static str] {
        match self {
            Admin::Layer => &["layer"],
            Admin::Document(_) => &["name"],
            Admin::Mount => &["collab", "layer"],
            Admin::Collab => &["collab"],
            Admin::Enrollment => &["fingerprint"],
            _ => &[],
        }
    }

    /// The faces it answers, its default first.
    fn faces(self) -> Vec<&'static str> {
        match self {
            Admin::Layer | Admin::Collab | Admin::Mount => vec![TOML, JSON],
            Admin::Document(kind) => vec![media_of(kind)],
            Admin::Participants => vec![TURTLE, TEXT, JSON],
            Admin::Grants | Admin::Clients => vec![JSON],
            _ => vec![TEXT, JSON],
        }
    }

    fn id(self) -> String {
        let what = match self {
            Admin::Layers => "layers".into(),
            Admin::Layer => "layer".into(),
            Admin::Documents(kind) => format!("{}s", type_of(kind)),
            Admin::Document(kind) => type_of(kind).into(),
            Admin::Collabs => "collabs".into(),
            Admin::Mount => "mount".into(),
            Admin::Collab => "collab".into(),
            Admin::Participants => "participants".into(),
            Admin::Enrollments => "enrollments".into(),
            Admin::Enrollment => "enrollment".into(),
            Admin::Grants => "grants".into(),
            Admin::Clients => "clients".into(),
            Admin::Revision => "revision".into(),
        };
        format!("layers-admin-{what}")
    }

    fn title(self) -> String {
        match self {
            Admin::Layers => "Configuration: layers".into(),
            Admin::Layer => "Configuration: one layer's definition".into(),
            Admin::Documents(kind) => format!("Configuration: {} documents", kind.key()),
            Admin::Document(kind) => format!("Configuration: one {} document", kind.key()),
            Admin::Collabs => "Configuration: collaborations".into(),
            Admin::Mount => "Configuration: one mount".into(),
            Admin::Collab => "Configuration: one manifest".into(),
            Admin::Participants => "Configuration: participants".into(),
            Admin::Enrollments => "Configuration: enrolled certificates".into(),
            Admin::Enrollment => "Configuration: one enrolled certificate".into(),
            Admin::Grants => "Configuration: derived grants".into(),
            Admin::Clients => "Configuration: derived clients".into(),
            Admin::Revision => "Configuration: revision".into(),
        }
    }

    fn summary(self) -> String {
        let what = match self {
            Admin::Layers => "Every layer definition in layers/: one `layer` line each, with its \
                              resource, model, kind and the collaborations mounting it"
                .to_string(),
            Admin::Layer => "One layer's definition, layers/{layer}.toml, exactly as the file \
                             holds it, or the same keys as JSON"
                .to_string(),
            Admin::Documents(kind) => format!(
                "Every {} document a layer definition names: one `{}` line each, with its \
                 resource, its file in layers/ and the layers naming it",
                kind.key(),
                type_of(kind)
            ),
            Admin::Document(kind) => format!(
                "One {} document, by the name it is served under (its file name less {}), \
                 exactly as its file in layers/ holds it",
                kind.key(),
                kind.suffix()
            ),
            Admin::Collabs => "Every collaboration in collabs/: one `collab` line each, with its \
                               resource and the layers it mounts, then one `mount` line per mount"
                .to_string(),
            Admin::Mount => "One [[mount]] table of a manifest: the verbs each participant holds \
                             on one layer, as TOML or JSON"
                .to_string(),
            Admin::Collab => "One collaboration's manifest, collabs/{collab}.toml, exactly as the \
                              file holds it, or the same keys as JSON"
                .to_string(),
            Admin::Participants => "Every participant: the participants graph, the same answer as \
                                    urn:layers:participants; as text one `participant` line each"
                .to_string(),
            Admin::Enrollments => "Every certificate fingerprint enrollment.toml enrolls: one \
                                   `enrollment` line each, with the participant it names"
                .to_string(),
            Admin::Enrollment => "One certificate enrollment.toml enrolls, by its SHA-256 \
                                  fingerprint: the participant it names"
                .to_string(),
            Admin::Grants => "What grants.json holds, derived from the files now by the grants \
                              generator; never written here"
                .to_string(),
            Admin::Clients => "What clients.json holds, derived from the files now by the grants \
                               generator; never written here"
                .to_string(),
            Admin::Revision => "The configuration's revision: a SHA-256 over the files it reads, \
                                in a fixed order, changing when any of them changes and not \
                                otherwise"
                .to_string(),
        };
        format!(
            "{what}. Read from the files on every request, for whoever administers this host \
             (urn:cap:layers:admin:read); no layer's entries"
        )
    }
}

/// One configuration resource, read-only.
pub(super) struct AdminEndpoint {
    host: Arc<Host>,
    admin: Admin,
    id: String,
}

impl AdminEndpoint {
    pub(super) fn new(host: &Arc<Host>, admin: Admin) -> AdminEndpoint {
        AdminEndpoint {
            host: Arc::clone(host),
            admin,
            id: admin.id(),
        }
    }

    fn dir(&self) -> &std::path::Path {
        self.host.config_dir()
    }

    fn config(&self) -> Result<Config> {
        Config::load(self.dir()).map_err(host_error)
    }

    async fn answer(&self, inv: &Invocation<'_>, face: &str) -> Result<Representation> {
        let key = |name: &str| inv.bindings.get(name).unwrap_or_default().to_string();
        match self.admin {
            Admin::Layers => {
                let config = self.config()?;
                let rows: Vec<serde_json::Value> = config
                    .layers
                    .values()
                    .map(|def| {
                        let mounted: Vec<&str> = config
                            .collabs
                            .values()
                            .filter(|m| m.mounts.iter().any(|mount| mount.layer == def.name))
                            .map(|m| m.name.as_str())
                            .collect();
                        serde_json::json!({
                            "name": def.name,
                            "resource": layer_resource(&def.name),
                            "model": def.model.as_str(),
                            "kind": def.kind.map(|k| k.notation()),
                            "mountedBy": mounted,
                        })
                    })
                    .collect();
                Ok(listing(face, "layers", rows, |row| {
                    format!(
                        "layer {} model={} kind={} mounted-by={}",
                        row["resource"].as_str().unwrap_or_default(),
                        row["model"].as_str().unwrap_or_default(),
                        row["kind"].as_str().unwrap_or("none"),
                        words(&row["mountedBy"]),
                    )
                }))
            }
            Admin::Layer => {
                let layer = key("layer");
                if !is_layer_name(&layer) {
                    return Err(Error::InvalidArgument {
                        name: "layer".into(),
                        detail: format!(
                            "`{layer}` is not a layer name: lowercase letters, digits and \
                             hyphens, at most {} characters",
                            crate::name::MAX_LAYER_NAME
                        ),
                    });
                }
                let file = format!("layers/{layer}.toml");
                let text = self.file(&file, || {
                    format!("no layer `{layer}` is defined: {file} does not exist")
                })?;
                toml_face(face, &file, text)
            }
            Admin::Documents(kind) => {
                let rows: Vec<serde_json::Value> = self
                    .documents(kind)?
                    .into_iter()
                    .map(|doc| {
                        serde_json::json!({
                            "name": doc.name,
                            "resource": document_resource(kind, &doc.name),
                            "file": format!("layers/{}", doc.file),
                            "layers": doc.layers,
                        })
                    })
                    .collect();
                let word = type_of(kind);
                Ok(listing(face, &format!("{word}s"), rows, |row| {
                    format!(
                        "{word} {} file={} layers={}",
                        row["resource"].as_str().unwrap_or_default(),
                        row["file"].as_str().unwrap_or_default(),
                        words(&row["layers"]),
                    )
                }))
            }
            Admin::Document(kind) => {
                let name = key("name");
                let Some(doc) = self.documents(kind)?.into_iter().find(|d| d.name == name) else {
                    return Err(Error::NotFound(format!(
                        "no layer definition names a {} document `{name}`: the documents are \
                         listed at {}",
                        kind.key(),
                        documents_resource(kind)
                    )));
                };
                let file = format!("layers/{}", doc.file);
                let text = self.file(&file, || {
                    format!(
                        "{} document `{name}` is named by layer {}, and {file} does not exist",
                        kind.key(),
                        doc.layers.iter().cloned().collect::<Vec<_>>().join(", ")
                    )
                })?;
                Ok(Representation::new(ReprType::new(media_of(kind)), text))
            }
            Admin::Collabs => {
                let config = self.config()?;
                let rows: Vec<serde_json::Value> = config
                    .collabs
                    .values()
                    .map(|m| {
                        serde_json::json!({
                            "name": m.name,
                            "resource": collab_resource(&m.name),
                            "label": m.label,
                            "mounts": m.mounts.iter().map(|mount| serde_json::json!({
                                "layer": mount.layer,
                                "resource": mount_resource(&m.name, &mount.layer),
                            })).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                Ok(listing(face, "collabs", rows, |row| {
                    let mounts = row["mounts"].as_array().cloned().unwrap_or_default();
                    let layers: Vec<serde_json::Value> =
                        mounts.iter().map(|m| m["layer"].clone()).collect();
                    let mut text = format!(
                        "collab {} mounts={}",
                        row["resource"].as_str().unwrap_or_default(),
                        words(&serde_json::Value::Array(layers)),
                    );
                    for m in &mounts {
                        text.push_str(&format!(
                            "\nmount {}",
                            m["resource"].as_str().unwrap_or_default()
                        ));
                    }
                    text
                }))
            }
            Admin::Collab => {
                let collab = key("collab");
                let file = self.collab_file(&collab)?;
                let text = self.file(&file, || {
                    format!("no collaboration `{collab}`: {file} does not exist")
                })?;
                toml_face(face, &file, text)
            }
            Admin::Mount => {
                let (collab, layer) = (key("collab"), key("layer"));
                let file = self.collab_file(&collab)?;
                let text = self.file(&file, || {
                    format!("no collaboration `{collab}`: {file} does not exist")
                })?;
                let table = parse_toml(&file, &text)?;
                let mount = table
                    .get("mount")
                    .and_then(toml::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(toml::Value::as_table)
                    .find(|m| m.get("layer").and_then(toml::Value::as_str) == Some(&layer))
                    .cloned()
                    .ok_or_else(|| {
                        Error::NotFound(format!(
                            "collaboration `{collab}` mounts no layer `{layer}`: {file} has no \
                             [[mount]] with layer = \"{layer}\""
                        ))
                    })?;
                if face == JSON {
                    return Ok(json_repr(
                        serde_json::to_value(&mount)
                            .map_err(|e| Error::Endpoint(format!("{file}: {e}")))?,
                    ));
                }
                let text =
                    toml::to_string(&mount).map_err(|e| Error::Endpoint(format!("{file}: {e}")))?;
                Ok(Representation::new(ReprType::new(TOML), text.into_bytes()))
            }
            Admin::Participants => {
                let participants =
                    crate::config::load_participants(self.dir()).map_err(host_error)?;
                if face == TURTLE {
                    return Ok(turtle(rdf::to_turtle(&participants.triples)));
                }
                let rows: Vec<serde_json::Value> = participants
                    .members
                    .values()
                    .map(|m| {
                        let name = m.participant.iri().strip_prefix(PARTICIPANT_PREFIX);
                        serde_json::json!({
                            "participant": m.participant.iri(),
                            "name": name,
                            "resource": name.map(participant_resource),
                            "types": m.types,
                            "label": m.label,
                        })
                    })
                    .collect();
                Ok(listing(face, "participants", rows, |row| {
                    let mut line = format!(
                        "participant {}",
                        row["participant"].as_str().unwrap_or_default()
                    );
                    if let Some(resource) = row["resource"].as_str() {
                        line.push_str(&format!(" resource={resource}"));
                    }
                    line
                }))
            }
            Admin::Enrollments => {
                let participants =
                    crate::config::load_participants(self.dir()).map_err(host_error)?;
                let enrolled = load_enrolled(self.dir(), &participants).map_err(host_error)?;
                let mut rows: Vec<(String, String)> = enrolled
                    .iter()
                    .flat_map(|(iri, fps)| fps.iter().map(move |fp| (fp.clone(), iri.clone())))
                    .collect();
                rows.sort();
                let rows = rows
                    .into_iter()
                    .map(|(fp, iri)| enrollment_json(&fp, &iri))
                    .collect();
                Ok(listing(face, "enrollments", rows, enrollment_line))
            }
            Admin::Enrollment => {
                let asked = key("fingerprint");
                let Some(fp) = fold_fingerprint(&asked) else {
                    return Err(Error::InvalidArgument {
                        name: "fingerprint".into(),
                        detail: format!(
                            "`{asked}` is not a SHA-256 fingerprint: 64 hex digits, colons \
                             allowed between them"
                        ),
                    });
                };
                let participants =
                    crate::config::load_participants(self.dir()).map_err(host_error)?;
                let enrolled = load_enrolled(self.dir(), &participants).map_err(host_error)?;
                let Some(iri) = enrolled
                    .iter()
                    .find(|(_, fps)| fps.contains(&fp))
                    .map(|(iri, _)| iri.clone())
                else {
                    return Err(Error::NotFound(format!(
                        "enrollment.toml enrolls no certificate {fp}"
                    )));
                };
                let row = enrollment_json(&fp, &iri);
                Ok(if face == JSON {
                    json_repr(row)
                } else {
                    text_repr(enrollment_line(&row))
                })
            }
            Admin::Grants | Admin::Clients => {
                let config = self.config()?;
                let fingerprints = load_fingerprints(self.dir(), &config).map_err(host_error)?;
                let enrollment = generate(&config, &fingerprints).map_err(host_error)?;
                let text = match self.admin {
                    Admin::Grants => enrollment.grants_json(),
                    _ => enrollment.clients_json(),
                };
                Ok(Representation::new(ReprType::new(JSON), text.into_bytes()))
            }
            Admin::Revision => {
                let revision = revision(self.dir()).map_err(host_error)?;
                Ok(if face == JSON {
                    json_repr(serde_json::json!({ "revision": revision }))
                } else {
                    text_repr(format!("revision {revision}"))
                })
            }
        }
    }

    fn documents(&self, kind: DocumentKind) -> Result<Vec<Document>> {
        Ok(documents(self.dir())
            .map_err(host_error)?
            .into_iter()
            .filter(|d| d.kind == kind)
            .collect())
    }

    fn collab_file(&self, collab: &str) -> Result<String> {
        if !crate::name::is_short_name(collab) {
            return Err(Error::InvalidArgument {
                name: "collab".into(),
                detail: format!(
                    "`{collab}` is not a collaboration name: lowercase letters, digits and \
                     hyphens, at most {} characters, not starting or ending with a hyphen",
                    crate::name::MAX_SHORT_NAME
                ),
            });
        }
        Ok(format!("collabs/{collab}.toml"))
    }

    /// The bytes of `file` (relative to the config dir), unchanged; absent is `missing`.
    fn file(&self, file: &str, missing: impl FnOnce() -> String) -> Result<Vec<u8>> {
        let path = self.dir().join(file);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(missing())),
            Err(e) => Err(Error::Endpoint(format!("read {}: {e}", path.display()))),
        }
    }
}

fn text_repr(mut text: String) -> Representation {
    text.push('\n');
    Representation::new(ReprType::new(TEXT), text.into_bytes())
}

/// A set's answer: `{"<key>": rows}` as JSON, or one line (or more) per row as text.
fn listing(
    face: &str,
    key: &str,
    rows: Vec<serde_json::Value>,
    line: impl Fn(&serde_json::Value) -> String,
) -> Representation {
    if face == JSON {
        return json_repr(serde_json::json!({ key: rows }));
    }
    let text: String = rows.iter().map(|row| format!("{}\n", line(row))).collect();
    Representation::new(ReprType::new(TEXT), text.into_bytes())
}

/// A list of names as one comma-separated word, `none` when empty.
fn words(list: &serde_json::Value) -> String {
    let names: Vec<&str> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(",")
    }
}

fn enrollment_json(fp: &str, iri: &str) -> serde_json::Value {
    serde_json::json!({
        "fingerprint": fp,
        "resource": enrollment_resource(fp),
        "participant": iri,
    })
}

fn enrollment_line(row: &serde_json::Value) -> String {
    format!(
        "enrollment {} participant={}",
        row["fingerprint"].as_str().unwrap_or_default(),
        row["participant"].as_str().unwrap_or_default()
    )
}

fn parse_toml(file: &str, bytes: &[u8]) -> Result<toml::Table> {
    std::str::from_utf8(bytes)
        .map_err(|e| e.to_string())
        .and_then(|text| text.parse::<toml::Table>().map_err(|e| e.to_string()))
        .map_err(|e| Error::Endpoint(format!("{file} does not parse as TOML: {e}")))
}

/// A whole TOML file: its bytes unchanged, or its keys as JSON.
fn toml_face(face: &str, file: &str, bytes: Vec<u8>) -> Result<Representation> {
    if face != JSON {
        return Ok(Representation::new(ReprType::new(TOML), bytes));
    }
    let table = parse_toml(file, &bytes)?;
    Ok(json_repr(
        serde_json::to_value(&table).map_err(|e| Error::Endpoint(format!("{file}: {e}")))?,
    ))
}

#[async_trait]
impl Endpoint for AdminEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "{} is read-only and answers Source alone, and this request is {:?}: no \
                 configuration resource is written yet; ask with Source",
                self.admin.iri(),
                inv.request.verb
            )));
        }
        let faces = self.admin.faces();
        let face = face(inv, &faces)?.to_string();
        self.answer(inv, &face).await
    }

    fn name(&self) -> &str {
        &self.id
    }

    fn describe(&self) -> Description {
        let faces = self.admin.faces();
        let mut description = Description::new(self.id.clone())
            .title(self.admin.title())
            .summary(self.admin.summary())
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_ADMIN_READ);
        for key in self.admin.keys() {
            description = description.input(
                ArgSpec::new(*key)
                    .summary(format!("the {key} the resource is keyed by"))
                    .class(xsd("string"))
                    .binding(),
            );
        }
        if faces.len() > 1 {
            description = description.input(as_spec(&faces));
        }
        for f in faces {
            description = description.output(f);
        }
        description
    }
}
