//! Path storage convention. Paths inside `music_dir` are stored in the DB
//! relative to that root; paths outside (inbox files, user-relocated copies)
//! are stored absolute. The result is that renaming the library directory
//! requires nothing more than updating `[library].music_dir` in the config
//! — every relative path resolves through the new prefix automatically.
//!
//! Inspired by beets v2.10 (see `beetbox/beets#133`).

use std::path::{Path, PathBuf};

/// Convert an arbitrary path to its DB-stored form.
///
/// - Absolute paths under `music_dir` are stripped to a relative path.
/// - Absolute paths outside `music_dir` are returned as-is.
/// - Relative paths pass through unchanged (treated as already in DB form).
///
/// The strip uses `Path::strip_prefix`, which is component-aware — so a
/// `music_dir` of `/foo/Music` will not accidentally strip the prefix of
/// `/foo/Music Backup/...`. When the lexical check misses because one side
/// uses a symlink or normalized filesystem spelling, an existing path falls
/// back to canonicalized comparison.
pub fn to_db_path(path: &Path, music_dir: &Path) -> String {
    // No music_dir context (in-memory tests, fresh setup) — every path is
    // stored verbatim; relative inputs stay relative.
    if path.is_relative() || music_dir.as_os_str().is_empty() {
        return path.display().to_string();
    }

    if let Some(rel) = strip_non_empty(path, music_dir) {
        return rel.display().to_string();
    }

    if let (Ok(canonical_path), Ok(canonical_music_dir)) =
        (std::fs::canonicalize(path), std::fs::canonicalize(music_dir))
        && let Some(rel) = strip_non_empty(&canonical_path, &canonical_music_dir)
    {
        return rel.display().to_string();
    }

    path.display().to_string()
}

fn strip_non_empty(path: &Path, prefix: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(prefix).ok()?;
    if rel.as_os_str().is_empty() {
        None
    } else {
        Some(rel.to_path_buf())
    }
}

/// Resolve a DB-stored path back to an absolute filesystem path.
/// Relative inputs are joined with `music_dir`; absolute inputs pass
/// through unchanged.
pub fn from_db_path(stored: &str, music_dir: &Path) -> PathBuf {
    let p = Path::new(stored);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    music_dir.join(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_music_dir_strips_to_relative() {
        let music = Path::new("/home/user/Music");
        let p = Path::new("/home/user/Music/Artist/Album/01.mp3");
        assert_eq!(to_db_path(p, music), "Artist/Album/01.mp3");
    }

    #[test]
    fn outside_music_dir_kept_absolute() {
        let music = Path::new("/home/user/Music");
        let p = Path::new("/home/user/Downloads/x.mp3");
        assert_eq!(to_db_path(p, music), "/home/user/Downloads/x.mp3");
    }

    #[test]
    fn equal_to_music_dir_kept_absolute() {
        // The library root itself is not a track; defensively don't produce
        // an empty string if someone passes it in.
        let music = Path::new("/home/user/Music");
        assert_eq!(to_db_path(music, music), "/home/user/Music");
    }

    #[test]
    fn boundary_prefix_not_confused_with_sibling_dir() {
        let music = Path::new("/home/user/Music");
        let p = Path::new("/home/user/Music Backup/Artist/01.mp3");
        // /home/user/Music is NOT a parent of /home/user/Music Backup/...
        assert_eq!(to_db_path(p, music), "/home/user/Music Backup/Artist/01.mp3");
    }

    #[test]
    fn relative_input_passes_through() {
        let music = Path::new("/home/user/Music");
        assert_eq!(to_db_path(Path::new("Artist/01.mp3"), music), "Artist/01.mp3");
    }

    #[test]
    fn from_db_relative_is_joined() {
        let music = Path::new("/home/user/Music");
        assert_eq!(
            from_db_path("Artist/Album/01.mp3", music),
            PathBuf::from("/home/user/Music/Artist/Album/01.mp3")
        );
    }

    #[test]
    fn from_db_absolute_passes_through() {
        let music = Path::new("/home/user/Music");
        assert_eq!(
            from_db_path("/elsewhere/x.mp3", music),
            PathBuf::from("/elsewhere/x.mp3")
        );
    }

    #[test]
    fn round_trip_under_music_dir() {
        let music = Path::new("/home/user/Music");
        let abs = PathBuf::from("/home/user/Music/A/B.mp3");
        let stored = to_db_path(&abs, music);
        assert_eq!(from_db_path(&stored, music), abs);
    }

    #[test]
    fn round_trip_outside_music_dir() {
        let music = Path::new("/home/user/Music");
        let abs = PathBuf::from("/tmp/inbox/x.mp3");
        let stored = to_db_path(&abs, music);
        assert_eq!(from_db_path(&stored, music), abs);
    }

    #[test]
    fn rename_music_dir_round_trip() {
        // The whole point of the refactor: the same DB row resolves under
        // a renamed music_dir without any DB rewrite.
        let stored = to_db_path(
            Path::new("/home/user/Music/Artist/01.mp3"),
            Path::new("/home/user/Music"),
        );
        let renamed = Path::new("/mnt/external/Music");
        assert_eq!(
            from_db_path(&stored, renamed),
            PathBuf::from("/mnt/external/Music/Artist/01.mp3")
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_fallback_strips_paths_when_music_dir_is_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let real_music = tmp.path().join("real_music");
        let link_music = tmp.path().join("link_music");
        let track = real_music.join("Artist/Album/01.flac");
        std::fs::create_dir_all(track.parent().unwrap()).unwrap();
        std::fs::write(&track, b"").unwrap();
        std::os::unix::fs::symlink(&real_music, &link_music).unwrap();

        assert_eq!(to_db_path(&track, &link_music), "Artist/Album/01.flac");
    }
}
