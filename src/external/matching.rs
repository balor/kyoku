use strsim::jaro_winkler;

use super::musicbrainz::MbRelease;

#[derive(Debug, Clone)]
pub struct MatchScore {
    pub total: f64,
    pub artist: f64,
    pub album: f64,
    pub track_count: f64,
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
    local_track_count: u32,
    local_track_titles: &[String],
    local_total_duration_ms: u64,
    candidate: &MbRelease,
) -> MatchScore {
    let artist = sim(local_artist, &candidate.artist);
    let album = sim(local_album, &candidate.title);

    // MB API returns a 0-100 relevance score that handles aliases, fuzzy
    // matching, and other heuristics we can't easily reproduce locally.
    let api_score = candidate.api_score as f64 / 100.0;

    // Track count: exact match = 1.0, each missing/extra track is a steep penalty.
    // This is an objective, reliable signal — 15 local tracks should strongly
    // prefer a 15-track release over a 13-track one.
    let track_count = if local_track_count == 0 || candidate.track_count == 0 {
        0.5
    } else if local_track_count == candidate.track_count {
        1.0
    } else {
        let diff = (local_track_count as i64 - candidate.track_count as i64).abs() as f64;
        // Penalty: each track difference costs ~0.15, so 2 tracks off → 0.70
        (1.0 - diff * 0.15).max(0.0)
    };

    // Per-track title similarity (ordered comparison)
    // None if data is unavailable — excluded from scoring rather than penalizing.
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

    // Duration comparison — None if data unavailable.
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
    // When duration or tracks are unavailable, redistribute their weight
    // proportionally among the factors that DO have data.
    let mut total_weight = 0.0f64;
    let mut weighted_sum = 0.0f64;

    // Always available
    let factors: &[(f64, f64)] = &[
        (api_score, 0.10),
        (artist, 0.15),
        (album, 0.20),
        (track_count, 0.25),
    ];
    for &(score, weight) in factors {
        weighted_sum += score * weight;
        total_weight += weight;
    }

    // Conditionally available
    if let Some(d) = duration {
        weighted_sum += d * 0.10;
        total_weight += 0.10;
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
        duration: duration.unwrap_or(0.0),
        tracks: tracks.unwrap_or(0.0),
    }
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
            1,
            &["Airbag".to_string()],
            240_000,
            &release,
        );
        // Artist mismatch should reduce the score noticeably compared to exact
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
            1,
            &["AIRBAG".to_string()],
            240_000,
            &release,
        );
        assert!(score.total > 0.9, "case should not matter: {}", score.total);
    }
}
