//! Bounded font discovery and user-only management shared by AppCore and ViewKit.
//! System/shared fonts are observations, never mutable targets of this API.
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_FACES: u32 = 64;
const MAX_FILES: usize = 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
static LOCAL_REVISION: AtomicU64 = AtomicU64::new(0);
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    System,
    Shared,
    User,
}
#[derive(Clone, Debug)]
pub struct Locations {
    pub system: Vec<PathBuf>,
    pub shared: Vec<PathBuf>,
    pub user: Option<PathBuf>,
}
impl Locations {
    pub fn current() -> Self {
        #[cfg(target_os = "mochios")]
        let result = Self {
            system: vec!["/system/libraries/fonts".into()],
            shared: vec!["/libraries/fonts".into()],
            user: std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local/share/mochios/fonts")),
        };
        #[cfg(not(target_os = "mochios"))]
        let result = {
            let mut system = vec![
                PathBuf::from("/usr/share/fonts"),
                PathBuf::from("/usr/local/share/fonts"),
            ];
            if let Some(path) = std::env::var_os("MOCHIOS_PREVIEW_FONTS_SYSTEM") {
                system.push(path.into());
            }
            let user = std::env::var_os("MOCHIOS_PREVIEW_CONFIG_ROOT")
                .map(|p| PathBuf::from(p).join("fonts"))
                .or_else(|| {
                    std::env::var_os("MOCHIOS_PREVIEW_ROOT")
                        .map(|p| PathBuf::from(p).join(".local/share/mochios/fonts"))
                });
            Self {
                system,
                shared: vec![],
                user,
            }
        };
        result
    }
    pub fn roots(&self) -> impl Iterator<Item = (Scope, &Path)> {
        self.system
            .iter()
            .map(|p| (Scope::System, p.as_path()))
            .chain(self.shared.iter().map(|p| (Scope::Shared, p.as_path())))
            .chain(self.user.iter().map(|p| (Scope::User, p.as_path())))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Face {
    pub index: u32,
    pub family: String,
    pub style: String,
    pub full_name: String,
    pub postscript_name: String,
    pub weight: u16,
    pub italic: bool,
    pub monospaced: bool,
    pub variable: bool,
    pub glyphs: u16,
    pub japanese: bool,
}
#[derive(Clone, Debug)]
pub struct FontFile {
    pub path: PathBuf,
    pub digest: String,
    pub scope: Scope,
    pub enabled: bool,
    pub bytes: u64,
    pub faces: Vec<Face>,
    pub managed: bool,
}
impl FontFile {
    /// A bounded, exact-file preview reference. Renderers validate location,
    /// digest and collection index before loading it, including disabled fonts.
    pub fn preview_family(&self, index: u32) -> String {
        format!(
            "mochi-font:{}:{index}:{}",
            self.digest,
            hex(self.path.to_string_lossy().as_bytes())
        )
    }
}
#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    pub files: Vec<FontFile>,
    pub warnings: Vec<String>,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn read_font(path: &Path) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FONT_BYTES {
        return Err(invalid("Not a regular supported-size font file."));
    }
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_FONT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FONT_BYTES {
        return Err(invalid("Font file exceeds 64 MiB."));
    }
    Ok(bytes)
}
fn name(face: &ttf_parser::Face<'_>, ids: &[u16]) -> Option<String> {
    for id in ids {
        let mut first = None;
        for n in face
            .names()
            .into_iter()
            .take(1024)
            .filter(|n| n.name_id == *id)
        {
            let Some(text) = n.to_string() else {
                continue;
            };
            let text = text
                .chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect::<String>()
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            if n.language_id == 0x409 {
                return Some(text);
            }
            first.get_or_insert(text);
        }
        if first.is_some() {
            return first;
        }
    }
    None
}
pub fn inspect(bytes: &[u8]) -> io::Result<Vec<Face>> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_FONT_BYTES {
        return Err(invalid("Font size is outside the supported range."));
    }
    let count = ttf_parser::fonts_in_collection(bytes).unwrap_or(1);
    if count == 0 || count > MAX_FACES {
        return Err(invalid("Font collection has too many faces."));
    }
    let mut result = vec![];
    for index in 0..count {
        let face = ttf_parser::Face::parse(bytes, index)
            .map_err(|_| invalid("This is not a valid OpenType or TrueType font."))?;
        if face.number_of_glyphs() == 0 {
            return Err(invalid("Font has no glyphs."));
        }
        let family =
            name(&face, &[16, 1]).ok_or_else(|| invalid("Font has no readable family name."))?;
        let style = name(&face, &[17, 2]).unwrap_or_else(|| "Regular".into());
        let full_name = name(&face, &[4]).unwrap_or_else(|| format!("{family} {style}"));
        result.push(Face {
            index,
            family,
            style,
            full_name,
            postscript_name: name(&face, &[6]).unwrap_or_default(),
            weight: face.weight().to_number(),
            italic: face.is_italic(),
            monospaced: face.is_monospaced(),
            variable: face.is_variable(),
            glyphs: face.number_of_glyphs(),
            japanese: face.glyph_index('あ').is_some() && face.glyph_index('漢').is_some(),
        });
    }
    Ok(result)
}
fn extension(path: &Path) -> Option<(&str, bool)> {
    let file = path.file_name()?.to_str()?;
    let (file, enabled) = file
        .strip_suffix(".disabled")
        .map_or((file, true), |f| (f, false));
    let ext = file.rsplit('.').next()?;
    ["ttf", "otf", "ttc", "otc"]
        .into_iter()
        .find(|valid| ext.eq_ignore_ascii_case(valid))
        .map(|ext| (ext, enabled))
}
fn managed_name(path: &Path, hash: &str) -> bool {
    let Some(file) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let file = file.strip_suffix(".disabled").unwrap_or(file);
    let Some((stem, _)) = file.rsplit_once('.') else {
        return false;
    };
    stem == hash
}
pub fn catalogue(locations: &Locations) -> Catalogue {
    let mut result = Catalogue::default();
    let mut paths = vec![];
    let mut total = 0u64;
    fn walk(
        path: &Path,
        scope: Scope,
        depth: usize,
        paths: &mut Vec<(Scope, PathBuf)>,
        warnings: &mut Vec<String>,
        remaining: &mut usize,
    ) {
        if paths.len() >= MAX_FILES || depth > 8 || *remaining == 0 {
            return;
        }
        *remaining -= 1;
        let metadata = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(_) => {
                warnings.push(format!("Could not read {}", path.display()));
                return;
            }
        };
        if metadata.file_type().is_symlink() {
            return;
        }
        if metadata.is_file() {
            if extension(path).is_some() {
                paths.push((scope, path.to_owned()));
            }
            return;
        }
        if !metadata.is_dir() {
            return;
        }
        match fs::read_dir(path) {
            Ok(entries) => {
                let mut entries = entries
                    .take(MAX_FILES * 8)
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .collect::<Vec<_>>();
                entries.sort();
                for entry in entries {
                    walk(&entry, scope, depth + 1, paths, warnings, remaining);
                    if paths.len() >= MAX_FILES || *remaining == 0 {
                        break;
                    }
                }
            }
            Err(_) => warnings.push(format!("Could not read {}", path.display())),
        }
    }
    let mut remaining = MAX_FILES * 8;
    let mut roots = locations.roots().collect::<Vec<_>>();
    roots.sort_by_key(|(scope, _)| if *scope == Scope::User { 0 } else { 1 });
    for (scope, path) in roots {
        walk(
            path,
            scope,
            0,
            &mut paths,
            &mut result.warnings,
            &mut remaining,
        );
    }
    if remaining == 0 || paths.len() >= MAX_FILES {
        result
            .warnings
            .push("The font scan reached its safety limit; some files were not listed.".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for (scope, path) in paths {
        let path = match fs::canonicalize(&path) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        let parsed = (|| {
            let bytes = read_font(&path)?;
            total = total
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| invalid("Font catalogue limit exceeded."))?;
            if total > MAX_TOTAL_BYTES {
                return Err(invalid("Font catalogue exceeds 512 MiB."));
            }
            let hash = digest(&bytes);
            let faces = inspect(&bytes)?;
            Ok((bytes.len() as u64, hash, faces))
        })();
        match parsed {
            Ok((bytes, hash, faces)) => {
                let enabled = extension(&path).unwrap().1;
                let managed = scope == Scope::User
                    && managed_name(&path, &hash)
                    && locations
                        .user
                        .as_ref()
                        .and_then(|p| fs::canonicalize(p).ok())
                        .as_deref()
                        == path.parent();
                result.files.push(FontFile {
                    path,
                    digest: hash,
                    scope,
                    enabled,
                    bytes,
                    faces,
                    managed,
                });
            }
            Err(error) => result.warnings.push(format!(
                "{}: {error}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
        if total >= MAX_TOTAL_BYTES {
            break;
        }
    }
    result
}
fn user_root(locations: &Locations, create: bool) -> io::Result<PathBuf> {
    let path = locations.user.as_ref().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "User font storage is unavailable.")
    })?;
    if !path.is_absolute() {
        return Err(invalid("User font folder must be absolute."));
    }
    if create {
        fs::create_dir_all(path)?;
    }
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(invalid("User font folder must be a real directory."));
    }
    fs::canonicalize(path)
}
fn temp_path(root: &Path, kind: &str) -> PathBuf {
    root.join(format!(
        ".{kind}-{}-{}.tmp",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ))
}
fn changed(root: &Path, action: &str, hash: &str) -> io::Result<()> {
    LOCAL_REVISION.fetch_add(1, Ordering::Release);
    let path = temp_path(root, "revision");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let result: io::Result<()> = (|| {
        write!(
            file,
            "{}:{}:{now}:{action}:{hash}",
            std::process::id(),
            TEMP_ID.load(Ordering::Relaxed)
        )?;
        file.sync_all()?;
        fs::rename(&path, root.join(".revision"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&path);
    }
    result
}
fn validate_target(locations: &Locations, file: &FontFile) -> io::Result<PathBuf> {
    if file.scope != Scope::User || !file.managed {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "System and external fonts cannot be changed.",
        ));
    }
    let root = user_root(locations, false)?;
    if file.scope != Scope::User
        || !file.managed
        || file.path.parent() != Some(root.as_path())
        || !managed_name(&file.path, &file.digest)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "System and external fonts cannot be changed.",
        ));
    }
    let data = read_font(&file.path)?;
    if digest(&data) != file.digest {
        return Err(invalid("Font file changed. Refresh before trying again."));
    }
    Ok(root)
}
pub fn install(locations: &Locations, source: &Path) -> io::Result<FontFile> {
    let data = read_font(source)?;
    let faces = inspect(&data)?;
    let hash = digest(&data);
    let root = user_root(locations, true)?;
    let ext = match data.get(..4) {
        Some(b"ttcf") => "ttc",
        Some(b"OTTO") => "otf",
        _ => "ttf",
    };
    let target = root.join(format!("{hash}.{ext}"));
    if target
        .with_file_name(format!("{hash}.{ext}.disabled"))
        .exists()
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "This font is already installed and disabled. Enable it from User Fonts.",
        ));
    }
    let temporary = temp_path(&root, "install");
    let result: io::Result<()> = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        output.write_all(&data)?;
        output.sync_all()?;
        // Atomic publication with no replacement of an existing font or link.
        fs::hard_link(&temporary, &target).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "This font file is already installed.",
                )
            } else {
                error
            }
        })?;
        changed(&root, "install", &hash).map_err(|error|io::Error::other(format!("The font was installed, but other apps could not be notified: {error}. Reopen other apps to refresh their fonts.")))?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result?;
    Ok(FontFile {
        path: target,
        digest: hash,
        scope: Scope::User,
        enabled: true,
        bytes: data.len() as u64,
        faces,
        managed: true,
    })
}
pub fn set_enabled(locations: &Locations, file: &FontFile, enabled: bool) -> io::Result<()> {
    let root = validate_target(locations, file)?;
    if file.enabled == enabled {
        return Ok(());
    }
    let name = file
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("Invalid font filename."))?;
    let target = root.join(if enabled {
        name.strip_suffix(".disabled")
            .ok_or_else(|| invalid("Font is not disabled."))?
            .to_owned()
    } else {
        format!("{name}.disabled")
    });
    fs::hard_link(&file.path, &target)?;
    if let Err(error) = fs::remove_file(&file.path) {
        let _ = fs::remove_file(&target);
        return Err(error);
    }
    changed(&root, if enabled { "enable" } else { "disable" }, &file.digest)
        .map_err(|error|io::Error::other(format!("The font state was changed, but other apps could not be notified: {error}. Reopen other apps to refresh their fonts.")))
}
pub fn remove(locations: &Locations, file: &FontFile) -> io::Result<()> {
    let root = validate_target(locations, file)?;
    fs::remove_file(&file.path)?;
    changed(&root, "remove", &file.digest).map_err(|error|io::Error::other(format!("The font was removed, but other apps could not be notified: {error}. Reopen other apps to refresh their fonts.")))
}
fn preview_reference(
    locations: &Locations,
    alias: &str,
) -> io::Result<Option<(String, u32, PathBuf)>> {
    let Some(alias) = alias.strip_prefix("mochi-font:") else {
        return Ok(None);
    };
    if alias.len() > 8192 {
        return Err(invalid("Preview reference is too long."));
    }
    let mut parts = alias.split(':');
    let hash = parts
        .next()
        .ok_or_else(|| invalid("Invalid font reference."))?;
    let index = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v < MAX_FACES)
        .ok_or_else(|| invalid("Invalid font index."))?;
    let path = parts.next().ok_or_else(|| invalid("Missing font path."))?;
    if hash.len() != 64
        || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        || parts.next().is_some()
        || path.len() % 2 != 0
        || !path.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid("Invalid font reference."));
    }
    let path = (0..path.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&path[i..i + 2], 16).map_err(|_| invalid("Invalid font path.")))
        .collect::<io::Result<Vec<_>>>()?;
    let path = PathBuf::from(String::from_utf8(path).map_err(|_| invalid("Invalid font path."))?);
    let canonical = fs::canonicalize(&path)?;
    if path != canonical
        || extension(&canonical).is_none()
        || !locations
            .roots()
            .any(|(_, root)| fs::canonicalize(root).is_ok_and(|root| canonical.starts_with(root)))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Font preview is outside the font library.",
        ));
    }
    Ok(Some((hash.into(), index, canonical)))
}
/// Validates exact-file preview aliases without permitting arbitrary file reads.
pub fn preview_data(locations: &Locations, alias: &str) -> io::Result<Option<(Vec<u8>, u32)>> {
    let Some((hash, index, path)) = preview_reference(locations, alias)? else {
        return Ok(None);
    };
    let data = read_font(&path)?;
    if digest(&data) != hash || inspect(&data)?.iter().all(|f| f.index != index) {
        return Err(invalid("Font preview has changed. Refresh the library."));
    }
    Ok(Some((data, index)))
}
/// Bounded preview-cache accounting; validates the location before inspecting it.
pub fn preview_size(locations: &Locations, alias: &str) -> Option<u64> {
    let (_, _, path) = preview_reference(locations, alias).ok()??;
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.is_file() && metadata.len() <= MAX_FONT_BYTES).then_some(metadata.len())
}
/// Lightweight change token. No font contents are read during frame polling.
pub fn revision() -> (u64, u64) {
    thread_local! {static CACHE:std::cell::RefCell<Option<(Instant,u64,u64)>>=const{std::cell::RefCell::new(None)};}
    let local = LOCAL_REVISION.load(Ordering::Acquire);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((at, last, stamp)) = *cache {
            if last == local && at.elapsed() < Duration::from_secs(1) {
                return (local, stamp);
            }
        }
        let bytes = Locations::current()
            .user
            .and_then(|root| File::open(root.join(".revision")).ok())
            .map(|f| {
                let mut bytes = vec![];
                let _ = f.take(256).read_to_end(&mut bytes);
                bytes
            })
            .unwrap_or_default();
        let stamp = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        });
        *cache = Some((Instant::now(), local, stamp));
        (local, stamp)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        locations: Locations,
        source: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mochios-fonts-test-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("system")).unwrap();
            let source = root.join("source.ttf");
            fs::write(
                &source,
                include_bytes!("../../viewkit/resources/fonts/InterVariable.ttf"),
            )
            .unwrap();
            fs::copy(&source, root.join("system/Inter.ttf")).unwrap();
            let locations = Locations {
                system: vec![root.join("system")],
                shared: vec![],
                user: Some(root.join("user")),
            };
            Self {
                root,
                locations,
                source,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn installation_preserves_original_and_never_replaces_an_existing_font() {
        let f = Fixture::new();
        let font = install(&f.locations, &f.source).unwrap();
        assert!(font.enabled && font.managed);
        assert_eq!(font.faces[0].family, "Inter");
        assert_eq!(fs::read(&f.source).unwrap(), fs::read(&font.path).unwrap());
        assert_eq!(
            install(&f.locations, &f.source).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(catalogue(&f.locations).files.len(), 2);
        let preview = preview_data(&f.locations, &font.preview_family(0))
            .unwrap()
            .unwrap();
        assert_eq!(preview.0, fs::read(&f.source).unwrap());
        assert_eq!(preview.1, 0);
    }
    #[test]
    fn disable_enable_and_remove_apply_to_the_whole_collection_file() {
        let f = Fixture::new();
        let font = install(&f.locations, &f.source).unwrap();
        set_enabled(&f.locations, &font, false).unwrap();
        assert!(!font.path.exists());
        let disabled = catalogue(&f.locations)
            .files
            .into_iter()
            .find(|f| f.scope == Scope::User)
            .unwrap();
        assert!(!disabled.enabled && disabled.managed);
        assert!(
            preview_data(&f.locations, &disabled.preview_family(0))
                .unwrap()
                .is_some()
        );
        set_enabled(&f.locations, &disabled, true).unwrap();
        assert!(font.path.exists());
        remove(&f.locations, &font).unwrap();
        assert!(!font.path.exists());
        assert!(f.source.exists());
    }
    #[test]
    fn protected_stale_and_outside_files_cannot_be_removed_or_previewed() {
        let f = Fixture::new();
        let system = catalogue(&f.locations).files.pop().unwrap();
        assert_eq!(
            remove(&f.locations, &system).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let font = install(&f.locations, &f.source).unwrap();
        fs::write(&font.path, b"changed").unwrap();
        assert!(remove(&f.locations, &font).is_err());
        assert!(font.path.exists());
        let outside = FontFile {
            path: fs::canonicalize(&f.source).unwrap(),
            ..system
        };
        assert!(preview_data(&f.locations, &outside.preview_family(0)).is_err());
        for invalid in [
            "mochi-font:x:0:x",
            "mochi-font:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx:0:ああ",
            "mochi-font:0000000000000000000000000000000000000000000000000000000000000000:999:00",
        ] {
            assert!(preview_data(&f.locations, invalid).is_err());
        }
    }
    #[test]
    fn malformed_fonts_and_excessive_collections_are_rejected_before_writing() {
        let f = Fixture::new();
        fs::write(&f.source, b"not a font").unwrap();
        assert!(install(&f.locations, &f.source).is_err());
        assert!(!f.locations.user.as_ref().unwrap().exists());
        OpenOptions::new()
            .write(true)
            .open(&f.source)
            .unwrap()
            .set_len(MAX_FONT_BYTES + 1)
            .unwrap();
        assert!(install(&f.locations, &f.source).is_err());
        assert!(!f.locations.user.as_ref().unwrap().exists());
        let mut ttc = vec![0; 32];
        ttc[..4].copy_from_slice(b"ttcf");
        ttc[4..8].copy_from_slice(&0x10000u32.to_be_bytes());
        ttc[8..12].copy_from_slice(&(MAX_FACES + 1).to_be_bytes());
        assert!(inspect(&ttc).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_storage_and_leaf_files_are_rejected() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        symlink(f.root.join("system"), f.locations.user.as_ref().unwrap()).unwrap();
        assert!(install(&f.locations, &f.source).is_err());
        let link = f.root.join("linked.ttf");
        symlink(&f.source, &link).unwrap();
        assert!(read_font(&link).is_err());
    }
}

#[cfg(test)]
mod collection_tests {
    use super::*;
    #[test]
    fn a_collection_keeps_each_face_index_and_is_managed_as_one_file() {
        let data = include_bytes!("../../viewkit/resources/fonts/InterVariable.ttf");
        let aligned = (data.len() + 3) & !3;
        let offsets = [32, 32 + aligned];
        let mut bytes = vec![0; 32 + aligned * 2];
        bytes[..4].copy_from_slice(b"ttcf");
        bytes[4..8].copy_from_slice(&0x10000u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&2u32.to_be_bytes());
        for (index, offset) in offsets.into_iter().enumerate() {
            bytes[12 + index * 4..16 + index * 4].copy_from_slice(&(offset as u32).to_be_bytes());
            bytes[offset..offset + data.len()].copy_from_slice(data);
            let count = u16::from_be_bytes(data[4..6].try_into().unwrap()) as usize;
            for table in 0..count {
                let field = offset + 12 + table * 16 + 8;
                let old = u32::from_be_bytes(bytes[field..field + 4].try_into().unwrap());
                bytes[field..field + 4].copy_from_slice(&(old + offset as u32).to_be_bytes());
            }
        }
        let faces = inspect(&bytes).unwrap();
        assert_eq!(faces.len(), 2);
        assert_eq!((faces[0].index, faces[1].index), (0, 1));
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    #[test]
    fn a_notification_failure_reports_that_the_font_was_already_published() {
        let root = std::env::temp_dir().join(format!(
            "mochios-font-notify-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("user/.revision")).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let source = root.join("source.ttf");
        fs::write(
            &source,
            include_bytes!("../../viewkit/resources/fonts/InterVariable.ttf"),
        )
        .unwrap();
        let locations = Locations {
            system: vec![],
            shared: vec![],
            user: Some(root.join("user")),
        };
        let error = install(&locations, &source).unwrap_err();
        assert!(error.to_string().contains("font was installed"));
        let catalogue = catalogue(&locations);
        assert_eq!(catalogue.files.len(), 1);
        assert!(catalogue.files[0].enabled);
    }
}

#[cfg(all(test, not(target_os = "mochios")))]
mod revision_tests {
    use super::*;
    #[test]
    #[ignore = "child-process fixture used by observes_external_revision_changes"]
    fn revision_child() {
        let before = revision().1;
        println!("FONT_TOKEN={before}");
        io::stdout().flush().unwrap();
        let mut line = String::new();
        io::stdin().read_line(&mut line).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        println!("FONT_TOKEN={}", revision().1);
        io::stdout().flush().unwrap();
    }
    #[test]
    fn observes_external_revision_changes() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let root = std::env::temp_dir().join(format!(
            "mochios-font-revision-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("fonts")).unwrap();
        fs::write(root.join("fonts/.revision"), b"before").unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "revision_tests::revision_child",
                "--ignored",
                "--nocapture",
            ])
            .env("MOCHIOS_PREVIEW_CONFIG_ROOT", &root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = io::BufReader::new(child.stdout.take().unwrap());
        fn token(stdout: &mut impl BufRead) -> String {
            loop {
                let mut line = String::new();
                assert!(stdout.read_line(&mut line).unwrap() > 0);
                if let Some((_, value)) = line.split_once("FONT_TOKEN=") {
                    return value.trim().to_owned();
                }
            }
        }
        let before = token(&mut stdout);
        fs::write(root.join("fonts/.revision"), b"after").unwrap();
        child.stdin.take().unwrap().write_all(b"next\n").unwrap();
        let after = token(&mut stdout);
        assert!(child.wait().unwrap().success());
        assert_ne!(before, after);
    }
}
