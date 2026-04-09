use strsim::jaro_winkler;

use super::musicbrainz::MbRelease;

#[derive(Debug, Clone)]
pub struct MatchScore {
    pub total: f64,
    pub artist: f64,
    pub album: f64,
    pub track_count: f64,
    pub year: f64,
    pub duration: f64,
    pub tracks: f64,
}

/// Score how well a local album group matches a MusicBrainz release candidate.
///
/// Returns a score from 0.0 to 1.0 where higher is better.
/// The caller provides the local metadata for comparison.
///
/// Weights:
/// - MB API score (0.10): incorporates aliases, fuzzy matching; but biased
///   toward the current canonical name, so kept low.
/// - Artist similarity (0.15): Jaro-Winkler on the name string
/// - Album similarity (0.20): Jaro-Winkler on the title string
/// - Track count (0.25): exact match is very strong signal — steepest penalty
///   for mismatch since it's the most objective metric we have from search.
/// - Duration (0.10): total duration within tolerance
/// - Per-track titles (0.20): ordered Jaro-Winkler (0.5 if unavailable)
pub fn score_release(
    local_artist: &str,
    local_album: &str,
    local_year: Option<i32>,
    local_track_count: u32,
    local_track_titles: &[String],
    local_total_duration_ms: u64,
    candidate: &MbRelease,
) -> MatchScore {
    let artist = sim(local_artist, &candidate.artist);

    // Normalize album titles before comparison: strip parenthesized suffixes
    // like "(Deluxe Edition)", normalize separators, collapse whitespace.
    let album = sim(
        &normalize_title(local_album),
        &normalize_title(&candidate.title),
    );

    // MB API returns a 0-100 relevance score that handles aliases, fuzzy
    // matching, and other heuristics we can't easily reproduce locally.
    let api_score = candidate.api_score as f64 / 100.0;

    // Track count: exact match = 1.0, each missing/extra track is a steep penalty.
    let track_count = if local_track_count == 0 || candidate.track_count == 0 {
        0.5
    } else if local_track_count == candidate.track_count {
        1.0
    } else {
        let diff = (local_track_count as i64 - candidate.track_count as i64).abs() as f64;
        (1.0 - diff * 0.15).max(0.0)
    };

    // Year: exact = 1.0, 1 off = 0.8, 2 off = 0.5, 3+ = proportional decay.
    // Excluded if either side is unknown.
    let year = match (local_year, candidate.year) {
        (Some(ly), Some(cy)) => {
            let diff = (ly - cy).unsigned_abs();
            Some(match diff {
                0 => 1.0,
                1 => 0.8,
                2 => 0.5,
                _ => (1.0 - diff as f64 * 0.2).max(0.0),
            })
        }
        _ => None,
    };

    // Per-track title similarity (ordered comparison).
    // Excluded if data is unavailable (search results don't include tracks).
    let tracks = if local_track_titles.is_empty() || candidate.tracks.is_empty() {
        None
    } else {
        let pairs = local_track_titles.len().min(candidate.tracks.len());
        let sum: f64 = local_track_titles
            .iter()
            .zip(candidate.tracks.iter())
            .map(|(local, mb)| sim(local, &mb.title))
            .sum();
        Some(sum / pairs as f64)
    };

    // Duration comparison. Excluded if data unavailable.
    let duration = if local_total_duration_ms == 0 {
        None
    } else {
        let mb_duration: u64 = candidate
            .tracks
            .iter()
            .filter_map(|t| t.duration_ms)
            .sum();
        if mb_duration == 0 {
            None
        } else {
            let diff = (local_total_duration_ms as f64 - mb_duration as f64).abs();
            let tolerance = (local_track_titles.len().max(1) * 10_000) as f64;
            Some((1.0 - diff / tolerance).max(0.0).min(1.0))
        }
    };

    // Weighted sum — only include factors that have real data.
    // When optional factors are unavailable, their weight is redistributed
    // proportionally among the factors that DO have data.
    let mut total_weight = 0.0f64;
    let mut weighted_sum = 0.0f64;

    // Always available
    let factors: &[(f64, f64)] = &[
        (api_score, 0.10),
        (artist, 0.15),
        (album, 0.15),
        (track_count, 0.20),
    ];
    for &(score, weight) in factors {
        weighted_sum += score * weight;
        total_weight += weight;
    }

    // Conditionally available
    if let Some(y) = year {
        weighted_sum += y * 0.15;
        total_weight += 0.15;
    }
    if let Some(d) = duration {
        weighted_sum += d * 0.05;
        total_weight += 0.05;
    }
    if let Some(t) = tracks {
        weighted_sum += t * 0.20;
        total_weight += 0.20;
    }

    let total = if total_weight > 0.0 {
        weighted_sum / total_weight
    } else {
        0.0
    };

    MatchScore {
        total,
        artist,
        album,
        track_count,
        year: year.unwrap_or(0.0),
        duration: duration.unwrap_or(0.0),
        tracks: tracks.unwrap_or(0.0),
    }
}

/// Normalize an album title for comparison: remove parentheses/brackets as
/// punctuation, normalize separators to spaces, collapse whitespace.
///
/// "MMXX (Hypa Hypa edition)" → "MMXX Hypa Hypa edition"
/// "MMXX - Hypa Hypa Edition" → "MMXX Hypa Hypa Edition"
/// "Rehab (bonus tracks version)" → "Rehab bonus tracks version"
fn normalize_title(s: &str) -> String {
    let normalized = s
        .replace('(', " ")
        .replace(')', " ")
        .replace('[', " ")
        .replace(']', " ")
        .replace(" - ", " ")
        .replace(" — ", " ")
        .replace(": ", " ");

    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Jaro-Winkler similarity, case-insensitive.
fn sim(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let a_lower: String = a.chars().flat_map(|c| c.to_lowercase()).collect();
    let b_lower: String = b.chars().flat_map(|c| c.to_lowercase()).collect();
    jaro_winkler(&a_lower, &b_lower)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::musicbrainz::{MbRelease, MbTrack};

    fn make_release(
        artist: &str,
        title: &str,
        tracks: &[&str],
    ) -> MbRelease {
        MbRelease {
            id: "test-id".to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            year: Some(1997),
            country: Some("GB".to_string()),
            label: None,
            track_count: tracks.len() as u32,
            tracks: tracks
                .iter()
                .enumerate()
                .map(|(i, t)| MbTrack {
                    position: (i + 1) as u32,
                    title: t.to_string(),
                    artist: None,
                    duration_ms: Some(240_000),
                    recording_id: String::new(),
                })
                .collect(),
            api_score: 100,
        }
    }

    #[test]
    fn exact_match_scores_high() {
        let release = make_release("Radiohead", "OK Computer", &["Airbag", "Paranoid Android"]);
        let score = score_release(
            "Radiohead",
            "OK Computer",
            Some(1997),
            2,
            &["Airbag".to_string(), "Paranoid Android".to_string()],
            480_000,
            &release,
        );
        assert!(score.total > 0.9, "expected > 0.9, got {}", score.total);
    }

    #[test]
    fn wrong_artist_scores_lower() {
        let release = make_release("Radiohead", "OK Computer", &["Airbag"]);
        let score = score_release(
            "Björk",
            "OK Computer",
            Some(1997),
            1,
            &["Airbag".to_string()],
            240_000,
            &release,
        );
        assert!(
            score.artist < 0.5,
            "artist score should be low: {}",
            score.artist
        );
        assert!(
            score.total < 0.95,
            "total should be below an exact match: {}",
            score.total
        );
    }

    #[test]
    fn case_insensitive_matching() {
        let release = make_release("radiohead", "ok computer", &["airbag"]);
        let score = score_release(
            "RADIOHEAD",
            "OK COMPUTER",
            Some(1997),
            1,
            &["AIRBAG".to_string()],
            240_000,
            &release,
        );
        assert!(score.total > 0.9, "case should not matter: {}", score.total);
    }

    #[test]
    fn year_mismatch_penalizes() {
        let release = make_release("Radiohead", "OK Computer", &["Airbag"]);
        let score_correct = score_release(
            "Radiohead", "OK Computer", Some(1997), 1,
            &["Airbag".to_string()], 240_000, &release,
        );
        let score_wrong = score_release(
            "Radiohead", "OK Computer", Some(2020), 1,
            &["Airbag".to_string()], 240_000, &release,
        );
        assert!(
            score_correct.total > score_wrong.total,
            "correct year {} should beat wrong year {}",
            score_correct.total, score_wrong.total
        );
    }

    #[test]
    fn normalized_album_title_matches() {
        let release = make_release("Artist", "MMXX Hypa Hypa Edition", &[]);
        let score = score_release(
            "Artist", "MMXX (Hypa Hypa edition)", None, 0,
            &[], 0, &release,
        );
        assert!(
            score.album > 0.9,
            "normalized titles should match well: {}",
            score.album
        );
    }
}
