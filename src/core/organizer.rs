use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::Settings;
use crate::core::template::{self, TemplateVars};
use crate::db::queries;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct FileMove {
    pub track_id: i64,
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Clone)]
pub struct FileCopy {
    pub track_id: i64,
    pub collection_id: i64,
    pub collection_name: String,
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Default)]
pub struct OrganizePlan {
    pub moves: Vec<FileMove>,
    pub copies: Vec<FileCopy>,
    pub skipped: usize,
}

#[derive(Debug, Default)]
pub struct OrganizeResult {
    pub moved: u32,
    pub copied: u32,
    pub errors: Vec<(String, String)>,
    pub dirs_cleaned: u32,
}

#[derive(Debug, Clone)]
pub enum OrganizeFilter {
    All,
    Artist(String),
    Album(String),
    Path(PathBuf),
    Collection(String),
}

/// Compute an organize plan without any side effects.
pub fn plan_organize(
    conn: &Connection,
    settings: &Settings,
    filter: OrganizeFilter,
) -> Result<OrganizePlan> {
    let music_dir = &settings.library.music_dir;
    let mut plan = OrganizePlan::default();

    let tracks = queries::get_all_tracks_for_organize(conn, &filter)?;

    for t in &tracks {
        let vars = TemplateVars {
            artist: t.artist.clone().unwrap_or_default(),
            album_artist: t.album_artist.clone().unwrap_or_default(),
            album: t.album_title.clone().unwrap_or_default(),
            year: t.year.map(|y| y.to_string()).unwrap_or_default(),
            title: t.title.clone(),
            track: t.track_number.unwrap_or(0),
            disc: t.disc_number,
            genre: t.genre.clone().unwrap_or_default(),
            ext: Path::new(&t.file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mp3")
                .to_lowercase(),
            label: t.label.clone().unwrap_or_default(),
            collection: String::new(),
        };

        // Choose template: single-disc or multi-disc
        let tmpl = if t.disc_total.unwrap_or(1) <= 1 {
            &settings.library.path_template_single_disc
        } else {
            &settings.library.path_template
        };

        let relative = template::render_path(tmpl, &vars);
        let target = music_dir.join(&relative);
        let from = PathBuf::from(&t.file_path);

        if from == target {
            plan.skipped += 1;
        } else {
            plan.moves.push(FileMove {
                track_id: t.id,
                from,
                to: target,
            });
        }

        // Collection copies
        for (coll_id, coll_name, coll_template) in &t.collections {
            if let Some(tmpl) = coll_template {
                let mut coll_vars = vars.clone();
                coll_vars.collection = coll_name.clone();
                let relative = template::render_path(tmpl, &coll_vars);
                let target = music_dir.join(&relative);

                plan.copies.push(FileCopy {
                    track_id: t.id,
                    collection_id: *coll_id,
                    collection_name: coll_name.clone(),
                    from: PathBuf::from(&t.file_path),
                    to: target,
                });
            }
        }
    }

    Ok(plan)
}

/// Execute an organize plan: move/copy files, update DB paths, clean up.
pub fn apply_organize(
    conn: &Connection,
    plan: &OrganizePlan,
    operation: &str,
) -> Result<OrganizeResult> {
    let mut result = OrganizeResult::default();
    let mut emptied_dirs: Vec<PathBuf> = Vec::new();

    for m in &plan.moves {
        if let Some(parent) = m.to.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let outcome = if operation == "copy" {
            std::fs::copy(&m.from, &m.to).map(|_| ())
        } else {
            std::fs::rename(&m.from, &m.to).or_else(|_| {
                // rename fails across filesystems — fallback to copy+delete
                std::fs::copy(&m.from, &m.to).map(|_| ())?;
                std::fs::remove_file(&m.from)
            })
        };

        match outcome {
            Ok(()) => {
                queries::update_track_path(conn, m.track_id, &m.to.display().to_string())?;
                result.moved += 1;

                // Track the source directory for cleanup
                if operation != "copy" {
                    if let Some(parent) = m.from.parent() {
                        emptied_dirs.push(parent.to_path_buf());
                    }
                }
            }
            Err(e) => {
                result.errors.push((m.from.display().to_string(), e.to_string()));
            }
        }
    }

    for c in &plan.copies {
        if let Some(parent) = c.to.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        match std::fs::copy(&c.from, &c.to) {
            Ok(_) => {
                queries::update_collection_track_path(
                    conn,
                    c.collection_id,
                    c.track_id,
                    &c.to.display().to_string(),
                )?;
                result.copied += 1;
            }
            Err(e) => {
                result.errors.push((c.from.display().to_string(), e.to_string()));
            }
        }
    }

    // Clean up empty source directories (deepest first)
    emptied_dirs.sort();
    emptied_dirs.dedup();
    emptied_dirs.reverse();
    for dir in &emptied_dirs {
        result.dirs_cleaned += remove_empty_parents(dir);
    }

    Ok(result)
}

/// Remove a directory and its empty parents, stopping at the first non-empty one.
fn remove_empty_parents(dir: &Path) -> u32 {
    let mut cleaned = 0u32;
    let mut current = dir.to_path_buf();
    loop {
        match std::fs::read_dir(&current) {
            Ok(mut entries) => {
                if entries.next().is_none() {
                    if std::fs::remove_dir(&current).is_ok() {
                        cleaned += 1;
                    } else {
                        break;
                    }
                } else {
                    break; // Not empty
                }
            }
            Err(_) => break,
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    cleaned
}
