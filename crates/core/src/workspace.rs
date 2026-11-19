//! Workspaces: a copy of a project on a machine, kept in step with the controller's.
//!
//! The controller lists the project's files with git (tracked and untracked, minus
//! ignored ones), hashes them and sends the list. The machine answers with the contents
//! it doesn't have yet, receives those, then makes its copy match: changed files are
//! replaced and files gone from the project are deleted. Everything else (ignored files
//! such as build folders and tool caches) is left alone, so builds stay warm.
//!
//! After a job, files matching output patterns can be listed and copied back.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::DeviceId;

pub type Hash = [u8; 32];

/// Most file data in one message, leaving room in the frame for everything else.
pub const CHUNK_BYTES: usize = 512 * 1024;
/// Most file entries in one message.
pub const ENTRIES_PER_MESSAGE: usize = 2000;
/// Most files one output listing may return.
pub const MAX_LISTED: usize = 20_000;

/// One file of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative to the project root, `/`-separated.
    pub path: String,
    pub size: u64,
    pub hash: Hash,
    pub executable: bool,
}

/// Part of a file's contents, sent to the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobPart {
    pub hash: Hash,
    pub offset: u64,
    /// The whole file's size.
    pub total: u64,
    pub data: Vec<u8>,
}

/// Names a workspace on a machine: letters, digits, `.`, `_` and `-` only, so it can
/// never name another folder.
#[derive(Debug, Clone, PartialEq, Eq, std::hash::Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorkspaceId(String);

impl WorkspaceId {
    pub fn parse(text: &str) -> Option<Self> {
        let valid = (1..=80).contains(&text.len())
            && !text.starts_with('.')
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        valid.then(|| WorkspaceId(text.to_string()))
    }

    /// `<folder name>-<8 hex>`: readable, and unique per controller and project folder.
    pub fn for_project(root: &Path, controller: &DeviceId) -> Self {
        let name: String = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .take(60)
            .collect();
        let name = name.trim_matches('-');
        let name = if name.is_empty() { "project" } else { name };
        let mut hasher = blake3::Hasher::new();
        hasher.update(controller.as_bytes());
        hasher.update(root.to_string_lossy().as_bytes());
        let tag = data_encoding::HEXLOWER.encode(&hasher.finalize().as_bytes()[..4]);
        WorkspaceId(format!("{name}-{tag}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for WorkspaceId {
    type Error = String;
    fn try_from(text: String) -> std::result::Result<Self, String> {
        WorkspaceId::parse(&text).ok_or_else(|| format!("invalid workspace name {text:?}"))
    }
}

impl From<WorkspaceId> for String {
    fn from(id: WorkspaceId) -> String {
        id.0
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a path from the other side is a plain relative path that stays inside the
/// workspace on every OS.
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', ':', '\0'])
                // Windows drops trailing dots and spaces, so "a." and "a" would collide.
                && !part.ends_with(['.', ' '])
        })
}

fn hex(hash: &Hash) -> String {
    data_encoding::HEXLOWER.encode(hash)
}

pub fn hash_file(path: &Path) -> std::io::Result<Hash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(std::fs::File::open(path)?)?;
    Ok(*hasher.finalize().as_bytes())
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    let mode = permissions.mode();
    let wanted = if executable {
        mode | ((mode & 0o444) >> 2)
    } else {
        mode & !0o111
    };
    if wanted != mode {
        permissions.set_mode(wanted);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> std::io::Result<()> {
    Ok(())
}

/// The path on this OS for a `/`-separated relative path under `root`.
fn join(root: &Path, path: &str) -> PathBuf {
    path.split('/')
        .fold(root.to_path_buf(), |p, part| p.join(part))
}

/// Writes `contents` from `source` to `target` through a temporary file, so a reader
/// never sees half a file.
fn replace_file(source: &mut impl Read, target: &Path) -> std::io::Result<()> {
    let parent = target.parent().expect("workspace files have a parent");
    std::fs::create_dir_all(parent)?;
    if target.is_dir() {
        std::fs::remove_dir_all(target)?;
    }
    let temp = parent.join(format!(
        ".{}.yonder-tmp",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut file = std::fs::File::create(&temp)?;
        std::io::copy(source, &mut file)?;
        file.flush()?;
    }
    std::fs::rename(&temp, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

// ---------------------------------------------------------------------------
// Controller side

/// A project folder on this device, ready to sync.
#[derive(Debug, Clone)]
pub struct LocalProject {
    /// The git repository's top folder.
    pub root: PathBuf,
    pub files: Vec<FileEntry>,
    /// Symlinks and other non-files that were left out.
    pub skipped: usize,
    /// Lines of `.yonderinclude` that matched nothing.
    pub missing: Vec<String>,
}

/// Lists files git ignores that should be synced anyway, one path or glob per line, such
/// as `.env` or signing keys a build needs. Lines starting with `#` are comments.
pub const INCLUDE_FILE: &str = ".yonderinclude";

impl LocalProject {
    /// Finds the git repository containing `start` and hashes its files: tracked and
    /// untracked, without ignored ones except those `.yonderinclude` lists.
    pub fn scan(start: &Path) -> Result<LocalProject> {
        let root = git(start, &["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(String::from_utf8_lossy(&root).trim());
        let listed = git(
            &root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;
        let (included, missing) = included_paths(&root)?;
        let mut seen = HashSet::new();
        let mut files = Vec::new();
        let mut skipped = 0;
        let listed = listed
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|raw| String::from_utf8_lossy(raw).into_owned());
        for path in listed.chain(included) {
            if !seen.insert(path.clone()) {
                continue;
            }
            let full = join(&root, &path);
            let meta = match std::fs::symlink_metadata(&full) {
                Ok(meta) => meta,
                // Deleted but not yet committed: not part of the project any more.
                Err(_) => continue,
            };
            if !meta.is_file() || !valid_path(&path) {
                skipped += 1;
                continue;
            }
            files.push(FileEntry {
                hash: hash_file(&full).map_err(|e| Error::io(&full, e))?,
                size: meta.len(),
                executable: is_executable(&meta),
                path,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(LocalProject {
            root,
            files,
            skipped,
            missing,
        })
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// The ignored files `.yonderinclude` asks for, and the lines that matched nothing.
fn included_paths(root: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let file = root.join(INCLUDE_FILE);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(e) => return Err(Error::io(&file, e)),
    };
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_start_matches('/'))
        .collect();
    let mut found = Vec::new();
    let mut missing = Vec::new();
    let mut globs = Vec::new();
    for line in lines {
        if line.contains(['*', '?', '[', '{']) {
            let glob = globset::Glob::new(line)
                .map_err(|e| Error::Protocol(format!("{INCLUDE_FILE}: bad pattern {line}: {e}")))?;
            globs.push((line, glob.compile_matcher()));
        } else if valid_path(line) && join(root, line).is_file() {
            found.push(line.to_string());
        } else {
            missing.push(line.to_string());
        }
    }
    if !globs.is_empty() {
        // Only patterns need the (possibly long) list of ignored files.
        let ignored = git(
            root,
            &[
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--exclude-standard",
            ],
        )?;
        let ignored: Vec<String> = ignored
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|raw| String::from_utf8_lossy(raw).into_owned())
            .collect();
        for (line, matcher) in globs {
            let before = found.len();
            found.extend(ignored.iter().filter(|p| matcher.is_match(p)).cloned());
            if found.len() == before {
                missing.push(line.to_string());
            }
        }
    }
    Ok((found, missing))
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| Error::Protocol(format!("couldn't run git: {e}")))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(Error::Protocol(
            if message.contains("not a git repository") {
                format!("{} isn't inside a git repository", dir.display())
            } else {
                format!("git failed: {message}")
            },
        ));
    }
    Ok(output.stdout)
}

/// Writes a pulled file into the local project.
pub fn write_local(root: &Path, entry: &FileEntry, data: &[u8]) -> Result<PathBuf> {
    let target = join(root, &entry.path);
    replace_file(&mut &data[..], &target).map_err(|e| Error::io(&target, e))?;
    set_executable(&target, entry.executable).map_err(|e| Error::io(&target, e))?;
    Ok(target)
}

/// Whether the local copy of a pulled file is already the same.
pub fn local_matches(root: &Path, entry: &FileEntry) -> bool {
    let target = join(root, &entry.path);
    std::fs::metadata(&target).is_ok_and(|m| m.is_file() && m.len() == entry.size)
        && hash_file(&target).is_ok_and(|h| h == entry.hash)
}

// ---------------------------------------------------------------------------
// Machine side

/// What the machine knows about a file in its copy, to avoid hashing it again.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Recorded {
    size: u64,
    mtime_ms: u64,
    hash: Hash,
}

type Records = HashMap<String, Recorded>;

/// The workspaces on this machine.
#[derive(Debug)]
pub struct Workspaces {
    /// Project copies, where jobs run.
    files_root: PathBuf,
    /// Bookkeeping: what each copy holds, a sync in progress, received contents.
    meta_root: PathBuf,
    lock: Mutex<()>,
}

impl Workspaces {
    pub fn new(files_root: impl Into<PathBuf>, meta_root: impl Into<PathBuf>) -> Self {
        Workspaces {
            files_root: files_root.into(),
            meta_root: meta_root.into(),
            lock: Mutex::new(()),
        }
    }

    /// The folder jobs see.
    pub fn dir(&self, id: &WorkspaceId) -> PathBuf {
        self.files_root.join(id.as_str())
    }

    fn meta(&self, id: &WorkspaceId) -> PathBuf {
        self.meta_root.join(id.as_str())
    }

    fn blob(&self, id: &WorkspaceId, hash: &Hash) -> PathBuf {
        self.meta(id).join("blobs").join(hex(hash))
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn load<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| postcard::from_bytes(&bytes).ok())
            .unwrap_or_default()
    }

    fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
        let bytes = postcard::to_stdvec(value).map_err(|e| Error::Protocol(e.to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        std::fs::write(path, bytes).map_err(|e| Error::io(path, e))
    }

    /// The hash of the copy's file at `path`, if it's there. Uses the record when the size
    /// and modification time haven't changed.
    fn current_hash(dir: &Path, records: &Records, path: &str) -> Option<Hash> {
        let full = join(dir, path);
        let meta = std::fs::symlink_metadata(&full).ok()?;
        if !meta.is_file() {
            return None;
        }
        if let Some(r) = records.get(path)
            && r.size == meta.len()
            && r.mtime_ms == mtime_ms(&meta)
        {
            return Some(r.hash);
        }
        hash_file(&full).ok()
    }

    /// Takes part of the project's file list (`first` starts a new sync) and returns the
    /// contents the machine still needs for it.
    pub fn plan(&self, id: &WorkspaceId, files: Vec<FileEntry>, first: bool) -> Result<Vec<Hash>> {
        if let Some(bad) = files.iter().find(|f| !valid_path(&f.path)) {
            return Err(Error::Protocol(format!("invalid path {:?}", bad.path)));
        }
        let _guard = self.guard();
        let meta = self.meta(id);
        let pending_file = meta.join("pending");
        let mut pending: Vec<FileEntry> = if first {
            let _ = std::fs::remove_dir_all(meta.join("blobs"));
            Vec::new()
        } else {
            Self::load(&pending_file)
        };
        let dir = self.dir(id);
        let records: Records = Self::load(&meta.join("records"));
        let mut on_disk: HashSet<Hash> = HashSet::new();
        let mut needed = Vec::new();
        let mut asked = HashSet::new();
        for file in &files {
            if Self::current_hash(&dir, &records, &file.path) == Some(file.hash) {
                on_disk.insert(file.hash);
                continue;
            }
            if self.blob(id, &file.hash).is_file() || asked.contains(&file.hash) {
                continue;
            }
            // The same contents elsewhere in the copy (a moved or duplicated file).
            let elsewhere = records.iter().any(|(path, r)| {
                r.hash == file.hash && Self::current_hash(&dir, &records, path) == Some(r.hash)
            });
            if on_disk.contains(&file.hash) || elsewhere {
                continue;
            }
            asked.insert(file.hash);
            needed.push(file.hash);
        }
        pending.extend(files);
        Self::save(&pending_file, &pending)?;
        Ok(needed)
    }

    /// Stores received contents.
    pub fn put(&self, id: &WorkspaceId, parts: Vec<BlobPart>) -> Result<()> {
        for part in parts {
            let target = self.blob(id, &part.hash);
            if target.is_file() {
                continue;
            }
            let partial = target.with_extension("part");
            std::fs::create_dir_all(target.parent().expect("blob has a parent"))
                .map_err(|e| Error::io(&target, e))?;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(part.offset == 0)
                .open(&partial)
                .map_err(|e| Error::io(&partial, e))?;
            file.seek(SeekFrom::Start(part.offset))
                .and_then(|_| file.write_all(&part.data))
                .map_err(|e| Error::io(&partial, e))?;
            drop(file);
            if part.offset + part.data.len() as u64 >= part.total {
                let hash = hash_file(&partial).map_err(|e| Error::io(&partial, e))?;
                if hash != part.hash {
                    let _ = std::fs::remove_file(&partial);
                    return Err(Error::Protocol(
                        "a file changed while it was being sent; sync again".into(),
                    ));
                }
                std::fs::rename(&partial, &target).map_err(|e| Error::io(&target, e))?;
            }
        }
        Ok(())
    }

    /// Makes the copy match the file list sent with [`Workspaces::plan`]. Returns the
    /// copy's folder.
    pub fn apply(&self, id: &WorkspaceId) -> Result<PathBuf> {
        let _guard = self.guard();
        let meta = self.meta(id);
        let pending: Vec<FileEntry> = Self::load(&meta.join("pending"));
        let mut records: Records = Self::load(&meta.join("records"));
        let dir = self.dir(id);
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |e| Error::io(&path, e)
        };

        // Files already right, and where each content can be found in the copy.
        let mut current: HashMap<&str, Hash> = HashMap::new();
        let mut sources: HashMap<Hash, String> = HashMap::new();
        for (path, r) in &records {
            if Self::current_hash(&dir, &records, path) == Some(r.hash) {
                sources.entry(r.hash).or_insert_with(|| path.clone());
            }
        }
        for file in &pending {
            if let Some(hash) = Self::current_hash(&dir, &records, &file.path) {
                current.insert(&file.path, hash);
                sources.entry(hash).or_insert_with(|| file.path.clone());
            }
        }
        let changes: Vec<&FileEntry> = pending
            .iter()
            .filter(|f| current.get(f.path.as_str()) != Some(&f.hash))
            .collect();

        // Copy contents that come from other files in the copy aside first, before any of
        // those files are replaced.
        for file in &changes {
            let blob = self.blob(id, &file.hash);
            if blob.is_file() {
                continue;
            }
            let Some(source) = sources.get(&file.hash) else {
                return Err(Error::Protocol(format!(
                    "the machine is missing the contents of {}; sync again",
                    file.path
                )));
            };
            let source = join(&dir, source);
            std::fs::create_dir_all(blob.parent().expect("blob has a parent"))
                .map_err(io(&blob))?;
            std::fs::copy(&source, &blob).map_err(io(&source))?;
        }

        // Remove files that left the project.
        let wanted: HashSet<&str> = pending.iter().map(|f| f.path.as_str()).collect();
        for path in records.keys().filter(|p| !wanted.contains(p.as_str())) {
            let full = join(&dir, path);
            if std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_file()) {
                std::fs::remove_file(&full).map_err(io(&full))?;
            }
            // Tidy folders this emptied, but never the workspace itself.
            let mut parent = full.parent();
            while let Some(folder) = parent {
                if folder == dir || std::fs::remove_dir(folder).is_err() {
                    break;
                }
                parent = folder.parent();
            }
        }

        for file in &changes {
            let blob = self.blob(id, &file.hash);
            let target = join(&dir, &file.path);
            let mut source = std::fs::File::open(&blob).map_err(io(&blob))?;
            replace_file(&mut source, &target).map_err(io(&target))?;
        }

        let mut new_records = Records::new();
        for file in &pending {
            let target = join(&dir, &file.path);
            set_executable(&target, file.executable).map_err(io(&target))?;
            let meta = std::fs::metadata(&target).map_err(io(&target))?;
            new_records.insert(
                file.path.clone(),
                Recorded {
                    size: meta.len(),
                    mtime_ms: mtime_ms(&meta),
                    hash: file.hash,
                },
            );
        }
        records = new_records;
        Self::save(&meta.join("records"), &records)?;
        let _ = std::fs::remove_file(meta.join("pending"));
        let _ = std::fs::remove_dir_all(meta.join("blobs"));
        Ok(dir)
    }

    /// Files in the copy matching any of `patterns` (globs relative to the project root,
    /// such as `build/windows/**`).
    pub fn list(&self, id: &WorkspaceId, patterns: &[String]) -> Result<Vec<FileEntry>> {
        let mut builder = globset::GlobSetBuilder::new();
        for pattern in patterns {
            let glob = globset::GlobBuilder::new(pattern.trim_start_matches("./"))
                .literal_separator(true)
                .build()
                .map_err(|e| Error::Protocol(format!("bad pattern {pattern:?}: {e}")))?;
            builder.add(glob);
        }
        let set = builder
            .build()
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let dir = self.dir(id);
        let mut found = Vec::new();
        let mut stack = vec![(dir.clone(), String::new())];
        while let Some((folder, prefix)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                if meta.is_dir() {
                    stack.push((entry.path(), path));
                } else if meta.is_file() && set.is_match(&path) && valid_path(&path) {
                    if found.len() >= MAX_LISTED {
                        return Err(Error::Protocol(format!(
                            "more than {MAX_LISTED} files match; narrow the pattern"
                        )));
                    }
                    found.push(FileEntry {
                        hash: hash_file(&entry.path()).map_err(|e| Error::io(entry.path(), e))?,
                        size: meta.len(),
                        executable: is_executable(&meta),
                        path,
                    });
                }
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    /// Up to [`CHUNK_BYTES`] of a file in the copy, from `offset`.
    pub fn read(&self, id: &WorkspaceId, path: &str, offset: u64) -> Result<Vec<u8>> {
        if !valid_path(path) {
            return Err(Error::Protocol(format!("invalid path {path:?}")));
        }
        let full = join(&self.dir(id), path);
        let mut file = std::fs::File::open(&full).map_err(|e| Error::io(&full, e))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| Error::io(&full, e))?;
        let mut data = Vec::with_capacity(CHUNK_BYTES);
        file.take(CHUNK_BYTES as u64)
            .read_to_end(&mut data)
            .map_err(|e| Error::io(&full, e))?;
        Ok(data)
    }

    /// Whether `controller` may use the copy. The first device to use a copy owns it;
    /// others are refused.
    pub fn claim(&self, id: &WorkspaceId, controller: &DeviceId) -> Result<bool> {
        let _guard = self.guard();
        let path = self.meta(id).join(OWNER);
        match std::fs::read_to_string(&path) {
            Ok(owner) => Ok(owner.trim() == controller.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let meta = self.meta(id);
                std::fs::create_dir_all(&meta).map_err(|e| Error::io(&meta, e))?;
                std::fs::write(&path, controller.to_string()).map_err(|e| Error::io(&path, e))?;
                Ok(true)
            }
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// The copies `controller` owns, with their size on disk (build output included).
    pub fn owned_by(&self, controller: &DeviceId) -> Vec<WorkspaceInfo> {
        let owner = controller.to_string();
        let Ok(entries) = std::fs::read_dir(&self.meta_root) else {
            return Vec::new();
        };
        let mut found: Vec<WorkspaceInfo> = entries
            .flatten()
            .filter_map(|e| WorkspaceId::parse(e.file_name().to_str()?))
            .filter(|id| {
                std::fs::read_to_string(self.meta(id).join(OWNER)).is_ok_and(|o| o.trim() == owner)
            })
            .map(|id| {
                let dir = self.dir(&id);
                let (files, bytes) = folder_size(&dir);
                let last_synced_ms = std::fs::metadata(self.meta(&id).join("records"))
                    .ok()
                    .map(|m| mtime_ms(&m));
                WorkspaceInfo {
                    path: dir.to_string_lossy().into_owned(),
                    id,
                    files,
                    bytes,
                    last_synced_ms,
                }
            })
            .collect();
        found.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        found
    }

    /// Deletes a copy and its bookkeeping; returns the bytes freed.
    pub fn remove(&self, id: &WorkspaceId) -> Result<u64> {
        let _guard = self.guard();
        let dir = self.dir(id);
        let (_, bytes) = folder_size(&dir);
        // Links inside (such as .fvm/flutter_sdk) are removed, not followed.
        for path in [dir, self.meta(id)] {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(&path, e)),
            }
        }
        Ok(bytes)
    }
}

const OWNER: &str = "owner";

/// A project copy on a machine, as its owner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: WorkspaceId,
    pub path: String,
    pub files: u64,
    pub bytes: u64,
    pub last_synced_ms: Option<u64>,
}

/// Files and bytes under `dir`, without following links.
fn folder_size(dir: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0, 0);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    (files, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, data: &[u8]) -> FileEntry {
        FileEntry {
            path: path.into(),
            size: data.len() as u64,
            hash: *blake3::hash(data).as_bytes(),
            executable: false,
        }
    }

    fn part(data: &[u8]) -> BlobPart {
        BlobPart {
            hash: *blake3::hash(data).as_bytes(),
            offset: 0,
            total: data.len() as u64,
            data: data.to_vec(),
        }
    }

    fn setup() -> (tempfile::TempDir, Workspaces, WorkspaceId) {
        let temp = tempfile::tempdir().unwrap();
        let ws = Workspaces::new(temp.path().join("files"), temp.path().join("meta"));
        (temp, ws, WorkspaceId::parse("app-1234abcd").unwrap())
    }

    #[test]
    fn paths_stay_inside() {
        for good in ["a", "lib/main.dart", ".github/workflows/ci.yml", "a b/c"] {
            assert!(valid_path(good), "{good}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "C:/x",
            "a\\b",
            "a/./b",
            "x.",
            "x ",
        ] {
            assert!(!valid_path(bad), "{bad}");
        }
        assert!(WorkspaceId::parse("../x").is_none());
        assert!(WorkspaceId::parse(".hidden").is_none());
        assert!(WorkspaceId::parse("a/b").is_none());
    }

    #[test]
    fn include_file_adds_ignored_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(root.join(".gitignore"), ".env\n*.pem\nbuild/\n").unwrap();
        std::fs::write(root.join(".env"), "API_URL=x").unwrap();
        std::fs::write(root.join("key.pem"), "secret").unwrap();
        std::fs::create_dir(root.join("build")).unwrap();
        std::fs::write(root.join("build/out.bin"), "big").unwrap();
        std::fs::write(root.join("main.txt"), "code").unwrap();

        let paths = |p: &LocalProject| p.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
        let plain = LocalProject::scan(root).unwrap();
        assert_eq!(paths(&plain), [".gitignore", "main.txt"]);

        std::fs::write(
            root.join(INCLUDE_FILE),
            "# needed by the build\n.env\n/*.pem\nmissing.txt\nnone/*.key\n",
        )
        .unwrap();
        let with = LocalProject::scan(root).unwrap();
        assert_eq!(
            paths(&with),
            [
                ".env",
                ".gitignore",
                ".yonderinclude",
                "key.pem",
                "main.txt"
            ]
        );
        assert_eq!(with.missing, ["missing.txt", "none/*.key"]);
    }

    #[test]
    fn first_device_owns_a_copy() {
        let (_temp, ws, id) = setup();
        let (mac, laptop) = (
            crate::identity::DeviceKey::generate().id(),
            crate::identity::DeviceKey::generate().id(),
        );
        assert!(ws.claim(&id, &mac).unwrap());
        assert!(ws.claim(&id, &mac).unwrap());
        assert!(!ws.claim(&id, &laptop).unwrap());

        ws.plan(&id, vec![entry("a.txt", b"hello")], true).unwrap();
        ws.put(&id, vec![part(b"hello")]).unwrap();
        ws.apply(&id).unwrap();
        let owned = ws.owned_by(&mac);
        assert_eq!(owned.len(), 1);
        assert_eq!((owned[0].files, owned[0].bytes), (1, 5));
        assert!(owned[0].last_synced_ms.is_some());
        assert!(ws.owned_by(&laptop).is_empty());

        assert_eq!(ws.remove(&id).unwrap(), 5);
        assert!(ws.owned_by(&mac).is_empty());
        assert!(!ws.dir(&id).exists());
        // Gone, so the next device to use the id owns it.
        assert!(ws.claim(&id, &laptop).unwrap());
    }

    #[test]
    fn sync_sends_only_whats_missing_and_keeps_ignored_files() {
        let (_temp, ws, id) = setup();
        let files = vec![entry("a.txt", b"one"), entry("src/b.txt", b"two")];
        let needed = ws.plan(&id, files.clone(), true).unwrap();
        assert_eq!(needed.len(), 2);
        ws.put(&id, vec![part(b"one"), part(b"two")]).unwrap();
        let dir = ws.apply(&id).unwrap();
        assert_eq!(std::fs::read(dir.join("src/b.txt")).unwrap(), b"two");

        // A build leaves output behind; it must survive the next sync.
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::write(dir.join("build/out.bin"), b"cache").unwrap();

        // Change one file, delete the other, add a copy of existing contents.
        let files = vec![entry("a.txt", b"ONE"), entry("copy.txt", b"one")];
        let needed = ws.plan(&id, files, true).unwrap();
        assert_eq!(needed, vec![*blake3::hash(b"ONE").as_bytes()]);
        ws.put(&id, vec![part(b"ONE")]).unwrap();
        ws.apply(&id).unwrap();
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"ONE");
        assert_eq!(std::fs::read(dir.join("copy.txt")).unwrap(), b"one");
        assert!(!dir.join("src").exists(), "emptied folders are removed");
        assert_eq!(std::fs::read(dir.join("build/out.bin")).unwrap(), b"cache");
    }

    #[test]
    fn files_changed_on_the_machine_are_sent_again() {
        let (_temp, ws, id) = setup();
        let files = vec![entry("gen.txt", b"from git")];
        ws.plan(&id, files.clone(), true).unwrap();
        ws.put(&id, vec![part(b"from git")]).unwrap();
        let dir = ws.apply(&id).unwrap();
        // A build rewrites a tracked file.
        std::fs::write(dir.join("gen.txt"), b"rewritten by the build").unwrap();
        let needed = ws.plan(&id, files, true).unwrap();
        assert_eq!(needed.len(), 1);
    }

    #[test]
    fn large_files_arrive_in_parts_and_bad_contents_are_refused() {
        let (_temp, ws, id) = setup();
        let data = vec![7u8; 1000];
        ws.plan(&id, vec![entry("big.bin", &data)], true).unwrap();
        let hash = *blake3::hash(&data).as_bytes();
        for (i, chunk) in data.chunks(300).enumerate() {
            ws.put(
                &id,
                vec![BlobPart {
                    hash,
                    offset: (i * 300) as u64,
                    total: 1000,
                    data: chunk.to_vec(),
                }],
            )
            .unwrap();
        }
        let dir = ws.apply(&id).unwrap();
        assert_eq!(std::fs::read(dir.join("big.bin")).unwrap(), data);

        let mut wrong = part(b"real");
        wrong.data = b"fake".to_vec();
        assert!(ws.put(&id, vec![wrong]).is_err());
    }

    #[test]
    fn apply_without_contents_fails_cleanly() {
        let (_temp, ws, id) = setup();
        ws.plan(&id, vec![entry("a.txt", b"never sent")], true)
            .unwrap();
        assert!(ws.apply(&id).is_err());
    }

    #[test]
    fn outputs_are_listed_and_read() {
        let (_temp, ws, id) = setup();
        let dir = ws.dir(&id);
        std::fs::create_dir_all(dir.join("build/release/data")).unwrap();
        std::fs::write(dir.join("build/release/app.exe"), b"exe").unwrap();
        std::fs::write(dir.join("build/release/data/app.so"), b"so").unwrap();
        std::fs::write(dir.join("build/other.txt"), b"x").unwrap();
        let found = ws.list(&id, &["build/release/**".into()]).unwrap();
        let paths: Vec<_> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["build/release/app.exe", "build/release/data/app.so"]
        );
        assert_eq!(ws.read(&id, "build/release/app.exe", 1).unwrap(), b"xe");
        assert!(ws.read(&id, "../escape", 0).is_err());
    }

    #[test]
    fn workspace_names_are_readable_and_distinct() {
        let key = crate::identity::DeviceKey::generate();
        let a = WorkspaceId::for_project(Path::new("/Users/me/Code/My App"), &key.id());
        let b = WorkspaceId::for_project(Path::new("/Users/me/Other/My App"), &key.id());
        assert!(a.as_str().starts_with("my-app-"), "{a}");
        assert_ne!(a, b);
        assert!(WorkspaceId::parse(a.as_str()).is_some());
    }
}
