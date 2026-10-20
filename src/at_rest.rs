//! Layer state, sealed at rest (ledger #2011).
//!
//! Every file the host writes under `<state>/{layer}/` holds [age](https://age-encryption.org/v1)
//! ciphertext (X25519), sealed to the host's own recipient and opened with the host's own
//! identity, so the files, a backup of them or a disk image are unreadable without the key:
//!
//! - an entry (`graphs/{author}/{id}.ttl`), a receipt (`receipts/{id}.ttl`, and a staged one
//!   under `receipts/.taking/`) and a tuple (`tuples/inbox/{cid}.tuple`) are each ONE age file;
//! - the feed (`feed.log`) stays a log of lines, so an append is still one synced write and a
//!   torn last line is still dropped on open: each line is one age file, base64 (standard
//!   alphabet, padded), so a line is `base64(age(event))`.
//!
//! age gives every file its own random file key, wrapped to the recipient, so this is
//! envelope encryption. The names keep their old suffixes (`.ttl`, `.tuple`, `.log`): what
//! is in a file says what it is (a sealed file begins `age-encryption.org/v1`).
//!
//! The KEY is an age identity file, as `age-keygen -o` writes it, handed to
//! [`crate::Host::open`] as a PATH ([`StateKey::load`]), so a deployment can hand it over
//! from an in-memory credential store. It may hold several identities: files are sealed to
//! the FIRST (the current one) and opened with any. That is rotation: put a new identity first,
//! keep the old one after it, open the host once with [`Migration::Reseal`] (every file sealed
//! only to an older identity is sealed again, to the new one), then drop the old line.
//!
//! What this does NOT protect against: the running host itself, which must read entries to
//! validate them and evaluate matches, and anyone who holds the identity file. End-to-end
//! sealing is separate work (ledger #2013).
//!
//! ```
//! use ikigai_layers::at_rest::{Migration, StateKey};
//!
//! # let dir = std::env::temp_dir().join(format!("ikigai-layers-key-doc-{}", std::process::id()));
//! # std::fs::create_dir_all(&dir).unwrap();
//! let path = dir.join("state.key");
//! // An operator runs `age-keygen -o state.key`; this writes the same file.
//! std::fs::write(&path, format!("# created by age-keygen\n{}\n",
//!     age::secrecy::ExposeSecret::expose_secret(&age::x25519::Identity::generate().to_string()))).unwrap();
//! let key = StateKey::load(&path).expect("an age identity file");
//! assert!(key.recipient().starts_with("age1"));
//! // A missing identity is refused, naming the path, never an empty layer.
//! let missing = StateKey::load(dir.join("absent.key")).unwrap_err();
//! assert!(missing.0.contains("absent.key") && missing.0.contains("does not exist"));
//! assert_eq!(Migration::default(), Migration::None);
//! # std::fs::remove_dir_all(&dir).unwrap();
//! ```

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use age::x25519::{Identity, Recipient};
use base64::Engine;

use crate::config::ConfigError;

/// The file at the root of a state directory that says which key the directory is sealed to:
/// a fixed text, sealed to the host's recipient. [`crate::Host::open`] refuses an identity that
/// cannot open it before reading anything else, so a wrong key never opens an empty layer, and
/// it is written once the first open with a key has succeeded.
pub const KEY_CHECK: &str = "key-check.age";

/// What [`KEY_CHECK`] holds.
const KEY_CHECK_TEXT: &[u8] = b"ikigai-layers: this state directory is sealed to this key\n";

/// The first line of every age file (age v1, binary).
pub const MAGIC: &[u8] = b"age-encryption.org/v1";

/// What every sealed feed line begins with: the base64 of [`MAGIC`] (21 bytes, so exactly 28
/// characters and no padding).
const LINE_MAGIC: &str = "YWdlLWVuY3J5cHRpb24ub3JnL3Yx";

/// What [`crate::Host::open_with`] does with state that is not sealed to the current key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Migration {
    /// Refuse a state directory that holds any plaintext file, naming it and this option.
    /// Files sealed to any identity in the key file open; nothing is rewritten.
    #[default]
    None,
    /// Seal every plaintext state file in place (the one-time migration from a host that
    /// wrote plaintext), and seal again, to the current identity, every file sealed only to
    /// an older one (rotation). File by file, each through a `.partial` file renamed over the
    /// old one, so a file is whole in its old form or its new one; an interrupted run is
    /// resumed by running it again. Each layer is opened again afterwards, refusing plaintext,
    /// so a migration that left any behind fails the open.
    Reseal,
}

/// The host's key: an age identity file, read once, at open.
///
/// Cheap to clone (one shared allocation). Its `Debug` names the file and the current
/// recipient, never a secret.
#[derive(Clone)]
pub struct StateKey(Arc<KeyInner>);

struct KeyInner {
    path: PathBuf,
    identities: Vec<Identity>,
    recipient: Recipient,
}

impl std::fmt::Debug for StateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateKey")
            .field("path", &self.0.path)
            .field("recipient", &self.recipient())
            .finish()
    }
}

impl StateKey {
    /// Read the age identity file at `path`: one `AGE-SECRET-KEY-1…` per line, blank lines
    /// and `#` comments skipped, the first the current identity. Refused, naming the path and
    /// never echoing a line of it: a file that does not exist, cannot be read or holds no
    /// identity, and a line that is not an X25519 age identity (by its number).
    pub fn load(path: impl AsRef<Path>) -> Result<StateKey, ConfigError> {
        let path = path.as_ref();
        let refuse = |why: String| {
            ConfigError(format!(
                "the state identity {}: {why}; the layer host seals its state to it and opens \
                 nothing without it",
                path.display()
            ))
        };
        let mut text = String::new();
        File::open(path)
            .and_then(|mut file| file.read_to_string(&mut text))
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => refuse(
                    "does not exist (an age identity file, as `age-keygen -o` writes one)"
                        .to_string(),
                ),
                _ => refuse(format!("cannot be read ({e})")),
            })?;
        let mut identities = Vec::new();
        for (number, line) in text.lines().enumerate().map(|(n, l)| (n + 1, l.trim())) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            identities.push(Identity::from_str(line).map_err(|e| {
                refuse(format!("line {number} is not an X25519 age identity ({e})"))
            })?);
        }
        let Some(current) = identities.first() else {
            return Err(refuse(
                "holds no age identity (expected a line AGE-SECRET-KEY-1…)".to_string(),
            ));
        };
        let recipient = current.to_public();
        Ok(StateKey(Arc::new(KeyInner {
            path: path.to_path_buf(),
            identities,
            recipient,
        })))
    }

    /// The identity file this key was read from.
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// The current recipient (`age1…`): what every file is sealed to now.
    pub fn recipient(&self) -> String {
        self.0.recipient.to_string()
    }
}

/// Why a file did not open.
#[derive(Debug)]
pub(crate) enum Unsealed {
    /// It is not age ciphertext: plaintext from before state was sealed, or something else.
    Plaintext,
    /// It is age ciphertext, and no identity in the key file opens it.
    NoMatchingKey,
    /// It begins as age ciphertext and does not open whole: torn, truncated or altered.
    Corrupt(String),
}

impl Unsealed {
    /// The reason, for a refusal that names `what`.
    pub(crate) fn about(&self, what: &Path) -> String {
        match self {
            Unsealed::Plaintext => format!(
                "{} is not sealed (plaintext from before layer state was encrypted at rest?): \
                 open the host once with the migration option (`Migration::Reseal`) to seal \
                 it in place",
                what.display()
            ),
            Unsealed::NoMatchingKey => format!(
                "{} is sealed to a key the state identity does not hold: the identity file \
                 lacks the key it was sealed to",
                what.display()
            ),
            Unsealed::Corrupt(why) => format!("{} does not open whole ({why})", what.display()),
        }
    }
}

/// How one host seals and opens its state files: its key, and whether a read accepts
/// plaintext (only while a [`Migration::Reseal`] opens a layer to seal it).
#[derive(Clone, Debug)]
pub struct Sealer {
    key: StateKey,
    plaintext: bool,
}

impl Sealer {
    /// Seal with `key`; every read refuses plaintext.
    pub fn new(key: &StateKey) -> Sealer {
        Sealer {
            key: key.clone(),
            plaintext: false,
        }
    }

    /// Seal with `key`; a read passes plaintext through, so state written before it was
    /// sealed can be read in order to seal it.
    pub(crate) fn migrating(key: &StateKey) -> Sealer {
        Sealer {
            key: key.clone(),
            plaintext: true,
        }
    }

    /// Seal `plaintext` to the current recipient, as binary age.
    pub(crate) fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let recipient = &self.key.0.recipient as &dyn age::Recipient;
        let encryptor = age::Encryptor::with_recipients(std::iter::once(recipient))
            .map_err(|e| format!("seal: {e}"))?;
        let mut sealed = Vec::with_capacity(plaintext.len() + 256);
        let mut writer = encryptor
            .wrap_output(&mut sealed)
            .map_err(|e| format!("seal: {e}"))?;
        writer
            .write_all(plaintext)
            .and_then(|()| writer.finish().map(|_| ()))
            .map_err(|e| format!("seal: {e}"))?;
        Ok(sealed)
    }

    /// Open a file's bytes with any identity in the key file (or pass plaintext through,
    /// migrating).
    ///
    /// A file that is empty or a proper prefix of an age header is a write that never
    /// completed (torn), never plaintext: [`Unsealed::Corrupt`].
    pub(crate) fn open(&self, bytes: &[u8]) -> Result<Vec<u8>, Unsealed> {
        if is_plaintext(bytes) {
            return if self.plaintext {
                Ok(bytes.to_vec())
            } else {
                Err(Unsealed::Plaintext)
            };
        }
        if !bytes.starts_with(MAGIC) {
            return Err(Unsealed::Corrupt(
                "torn: empty, or part of an age header".into(),
            ));
        }
        open_with(bytes, self.key.0.identities.iter())
    }

    /// Whether `bytes` are sealed and the CURRENT identity opens them: a file that needs no
    /// [`Migration::Reseal`].
    pub(crate) fn is_current(&self, bytes: &[u8]) -> bool {
        bytes.starts_with(MAGIC) && open_with(bytes, self.key.0.identities.iter().take(1)).is_ok()
    }

    /// Read the file at `path` and open it as UTF-8 text; the refusal names the file.
    pub(crate) fn read_text(&self, path: &Path) -> Result<String, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let opened = self.open(&bytes).map_err(|why| why.about(path))?;
        String::from_utf8(opened).map_err(|_| format!("{} is not UTF-8", path.display()))
    }

    /// Seal one feed line (without its newline) as `base64(age(line))`.
    pub(crate) fn seal_line(&self, line: &str) -> Result<String, String> {
        Ok(base64::engine::general_purpose::STANDARD.encode(self.seal(line.as_bytes())?))
    }

    /// Open one feed line (without its newline).
    pub(crate) fn open_line(&self, line: &str) -> Result<String, Unsealed> {
        if line_is_plaintext(line) {
            return if self.plaintext {
                Ok(line.to_string())
            } else {
                Err(Unsealed::Plaintext)
            };
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(line)
            .map_err(|e| Unsealed::Corrupt(format!("not base64: {e}")))?;
        let opened = open_with(&bytes, self.key.0.identities.iter())?;
        String::from_utf8(opened).map_err(|_| Unsealed::Corrupt("not UTF-8".into()))
    }

    /// Whether a feed line is sealed and the current identity opens it.
    pub(crate) fn is_current_line(&self, line: &str) -> bool {
        line.starts_with(LINE_MAGIC)
            && base64::engine::general_purpose::STANDARD
                .decode(line)
                .is_ok_and(|bytes| self.is_current(&bytes))
    }
}

fn open_with<'a>(
    bytes: &[u8],
    identities: impl Iterator<Item = &'a Identity>,
) -> Result<Vec<u8>, Unsealed> {
    let decryptor = age::Decryptor::new(bytes).map_err(|e| Unsealed::Corrupt(e.to_string()))?;
    let mut reader = decryptor
        .decrypt(identities.map(|i| i as &dyn age::Identity))
        .map_err(|e| match e {
            age::DecryptError::NoMatchingKeys => Unsealed::NoMatchingKey,
            other => Unsealed::Corrupt(other.to_string()),
        })?;
    let mut opened = Vec::new();
    reader
        .read_to_end(&mut opened)
        .map_err(|e| Unsealed::Corrupt(e.to_string()))?;
    Ok(opened)
}

/// Whether a file's first bytes say it is plaintext: non-empty, and not (a prefix of) an age
/// file. A torn write of a sealed file is a prefix of one, so it is not plaintext.
pub(crate) fn is_plaintext(bytes: &[u8]) -> bool {
    let n = bytes.len().min(MAGIC.len());
    n > 0 && bytes[..n] != MAGIC[..n]
}

/// Whether a feed's text holds a plaintext line (a torn last line included, judged the same
/// way as [`is_plaintext`]).
pub(crate) fn feed_is_plaintext(text: &str) -> bool {
    text.lines().any(line_is_plaintext)
}

fn line_is_plaintext(line: &str) -> bool {
    let n = line.len().min(LINE_MAGIC.len());
    n > 0 && line.as_bytes()[..n] != LINE_MAGIC.as_bytes()[..n]
}

/// Options for a file only this host reads: created `0600` on unix.
pub(crate) fn private_file() -> OpenOptions {
    #[allow(unused_mut)] // the mode is set on unix only
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Create `dir` (and its missing parents) `0700` on unix, and make `dir` itself `0700` if it
/// already existed with another mode.
pub(crate) fn private_dir(dir: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    set_mode(dir, 0o700)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let current = std::fs::symlink_metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions()
        .mode()
        & 0o7777;
    if current != mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Make a file this host wrote (or one a library it calls wrote for it) `0600`.
pub(crate) fn private(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    set_mode(path, 0o600)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Every directory under `root` (itself included) `0700` and every file `0600`, on unix:
/// state written before modes were set explicitly, and files a library the host calls
/// created with the process's umask. Symbolic links are neither followed nor changed.
pub(crate) fn tighten(root: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            set_mode(&dir, 0o700)?;
            let entries =
                std::fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
            for entry in entries {
                let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
                let kind = entry
                    .file_type()
                    .map_err(|e| format!("stat {}: {e}", entry.path().display()))?;
                if kind.is_dir() {
                    dirs.push(entry.path());
                } else if kind.is_file() {
                    set_mode(&entry.path(), 0o600)?;
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

/// Replace the file at `path` with `bytes`, atomically: written whole and synced to
/// `.{name}.partial` beside it (a name no reader takes for a record), then renamed over it.
/// A failure removes the partial file and leaves the old one as it was.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    let partial = path.with_file_name(format!(".{name}{}", crate::channel::PARTIAL));
    let written = private_file()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&partial)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .map_err(|e| format!("write {}: {e}", partial.display()))
        .and_then(|()| {
            std::fs::rename(&partial, path).map_err(|e| format!("replace {}: {e}", path.display()))
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    written
}

/// Check, and once an open has succeeded write, [`KEY_CHECK`] in `state_dir`.
pub(crate) struct KeyCheck {
    path: PathBuf,
    found: bool,
}

impl KeyCheck {
    /// Refuse a key that cannot open the state directory's [`KEY_CHECK`]; before anything
    /// else is read, so a wrong key opens nothing.
    pub(crate) fn verify(state_dir: &Path, key: &StateKey) -> Result<KeyCheck, String> {
        let path = state_dir.join(KEY_CHECK);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(KeyCheck { path, found: false })
            }
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        match Sealer::new(key).open(&bytes) {
            Ok(text) if text == KEY_CHECK_TEXT => Ok(KeyCheck { path, found: true }),
            Ok(_) => Err(format!(
                "{} opens, but does not hold what a layer host writes there",
                path.display()
            )),
            Err(Unsealed::NoMatchingKey) => Err(format!(
                "the state identity {} does not open this state directory: {} was sealed to \
                 another key (a wrong identity file, or one that lost the key this state was \
                 sealed to)",
                key.path().display(),
                path.display()
            )),
            Err(why) => Err(why.about(&path)),
        }
    }

    /// After a successful open: write the check if there was none, or seal it again to the
    /// current identity when `reseal` and it is sealed only to an older one.
    pub(crate) fn settle(&self, key: &StateKey, reseal: bool) -> Result<(), String> {
        let sealer = Sealer::new(key);
        let write = !self.found
            || (reseal && !std::fs::read(&self.path).is_ok_and(|bytes| sealer.is_current(&bytes)));
        if write {
            replace(&self.path, &sealer.seal(KEY_CHECK_TEXT)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_magic_is_the_base64_of_the_magic() {
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(MAGIC),
            LINE_MAGIC
        );
    }

    #[test]
    fn a_torn_sealed_file_is_not_plaintext() {
        assert!(!is_plaintext(b""));
        assert!(!is_plaintext(b"age-enc"));
        assert!(!is_plaintext(b"age-encryption.org/v1\n-> X25519"));
        assert!(is_plaintext(b"<urn:a> <urn:b> <urn:c> ."));
        assert!(!feed_is_plaintext("YWdlLWVu"));
        assert!(feed_is_plaintext(
            "1\t2026-10-10T00:00:00Z\tsay\turn:x\turn:y\n"
        ));
    }
}
