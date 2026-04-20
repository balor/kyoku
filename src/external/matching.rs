use strsim::jaro_winkler;

use super::musicbrainz::MbRelease;

#[derive(Debug, Clone)]
#[allow(dead_code)] // per-component fields used by unit tests for fine-grained assertions
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
    // When the two strings are in different scripts (e.g. "Yorushika" vs
    // "ヨルシカ", or "マエナラワナイ" vs "Maenarawanai" — same entity, one
    // is a romanization), Jaro-Winkler returns ~0 even though they refer to
    // the same thing. Treat this as "can't compare" rather than "different",
    // mirroring how `year` and `tracks` are excluded when unknown — the
    // remaining signals (track count, api_score, matching-script fields)
    // carry the weight. By this point release search has already resolved
    // the artist via MBID alias lookup, so a script mismatch here reflects
    // the MB credit being in a different script, not a wrong artist.
    let artist = if scripts_differ(local_artist, &candidate.artist) {
        None
    } else {
        Some(sim(local_artist, &candidate.artist))
    };

    // Normalize album titles before comparison: strip parenthesized suffixes
    // like "(Deluxe Edition)", normalize separators, collapse whitespace.
    let local_album_norm = normalize_title(local_album);
    let cand_album_norm = normalize_title(&candidate.title);
    let album = if scripts_differ(&local_album_norm, &cand_album_norm) {
        None
    } else {
        Some(sim(&local_album_norm, &cand_album_norm))
    };

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
        (track_count, 0.20),
    ];
    for &(score, weight) in factors {
        weighted_sum += score * weight;
        total_weight += weight;
    }

    // Conditionally available (excluded when scripts differ — same reasoning
    // as other optional factors)
    if let Some(a) = artist {
        weighted_sum += a * 0.15;
        total_weight += 0.15;
    }
    if let Some(a) = album {
        weighted_sum += a * 0.15;
        total_weight += 0.15;
    }
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
        artist: artist.unwrap_or(0.0),
        album: album.unwrap_or(0.0),
        track_count,
        year: year.unwrap_or(0.0),
        duration: duration.unwrap_or(0.0),
        tracks: tracks.unwrap_or(0.0),
    }
}

/// True when two strings share no common script — treating them as
/// comparable via Jaro-Winkler would produce a meaningless ~0 similarity.
///
/// Crucially this distinguishes *within* non-Latin scripts: "加藤隆" (CJK
/// ideographs) and "ヨルシカ" (Kana) are both "non-Latin" but reference
/// different scripts entirely, so they can't be compared either. Only
/// strings that share at least one script category (Latin, Kana, CJK,
/// Hangul, Cyrillic, …) are worth comparing.
fn scripts_differ(a: &str, b: &str) -> bool {
    let sa = scripts_of(a);
    let sb = scripts_of(b);
    // If either side has no script-bearing characters (e.g. pure digits or
    // punctuation), treat as comparable — sim() handles that case fine.
    if sa == 0 || sb == 0 {
        return false;
    }
    (sa & sb) == 0
}

// Bitset of scripts present in a string. Each bit is one script category.
// Digits, whitespace, and generic punctuation contribute nothing — a name
// like "Year 2023" is still Latin, and a pure "1999" has no script signal.
const S_LATIN: u16 = 1 << 0;
const S_KANA: u16 = 1 << 1;
const S_CJK: u16 = 1 << 2;
const S_HANGUL: u16 = 1 << 3;
const S_CYRILLIC: u16 = 1 << 4;
const S_GREEK: u16 = 1 << 5;
const S_HEBREW: u16 = 1 << 6;
const S_ARABIC: u16 = 1 << 7;
const S_THAI: u16 = 1 << 8;

fn scripts_of(s: &str) -> u16 {
    let mut set = 0u16;
    for c in s.chars() {
        let u = c as u32;
        if c.is_ascii_alphabetic() || (0x00C0..=0x024F).contains(&u) {
            set |= S_LATIN; // ASCII + Latin-1 Supplement + Latin Extended A/B
        } else if (0x3040..=0x30FF).contains(&u)
            || (0x31F0..=0x31FF).contains(&u)
            || (0xFF66..=0xFF9F).contains(&u)
        {
            set |= S_KANA; // Hiragana + Katakana (incl. phonetic ext + halfwidth)
        } else if (0x4E00..=0x9FFF).contains(&u)
            || (0x3400..=0x4DBF).contains(&u)
            || (0xF900..=0xFAFF).contains(&u)
        {
            set |= S_CJK; // CJK Unified Ideographs, Ext A, Compat
        } else if (0xAC00..=0xD7AF).contains(&u) || (0x1100..=0x11FF).contains(&u) {
            set |= S_HANGUL;
        } else if (0x0400..=0x04FF).contains(&u) {
            set |= S_CYRILLIC;
        } else if (0x0370..=0x03FF).contains(&u) {
            set |= S_GREEK;
        } else if (0x0590..=0x05FF).contains(&u) {
            set |= S_HEBREW;
        } else if (0x0600..=0x06FF).contains(&u) {
            set |= S_ARABIC;
        } else if (0x0E00..=0x0E7F).contains(&u) {
            set |= S_THAI;
        }
    }
    set
}

/// Normalize an album title for comparison: remove parentheses/brackets as
/// punctuation, normalize separators to spaces, collapse whitespace.
///
/// "MMXX (Hypa Hypa edition)" → "MMXX Hypa Hypa edition"
/// "MMXX - Hypa Hypa Edition" → "MMXX Hypa Hypa Edition"
/// "Rehab (bonus tracks version)" → "Rehab bonus tracks version"
fn normalize_title(s: &str) -> String {
    let normalized = s
        .replace(['(', ')', '[', ']'], " ")
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
            release_group_id: None,
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
    fn kanji_vs_kana_artist_is_treated_as_script_mismatch() {
        // File tag album_artist is "加藤隆" (pure Kanji/CJK), MB credit is
        // "ヨルシカ" (pure Katakana). Both are non-Latin but different
        // scripts — neither Jaro-Winkler nor a coarse "has non-Latin"
        // check can compare them usefully. Intersection of script sets
        // must be empty → artist factor excluded.
        let release = make_release("ヨルシカ", "幻燈", &[]);
        let score = score_release(
            "加藤隆",
            "幻燈",
            None,
            25,
            &[],
            0,
            &MbRelease {
                track_count: 25,
                year: Some(2023),
                ..release
            },
        );
        assert!(
            score.total > 0.95,
            "kanji vs kana artist should be script-disjoint and excluded: {}",
            score.total
        );
    }

    #[test]
    fn artist_script_mismatch_does_not_tank_score() {
        // Local tags: artist "Yorushika" (Latin), album "幻燈" (Kanji)
        // MB credit: artist "ヨルシカ" (Katakana), album "幻燈" (Kanji, same)
        // Same entity — artist is just credited in a different script. The
        // previous artist-MBID resolution in search_releases already
        // confirmed these are the same artist.
        let release = make_release("ヨルシカ", "幻燈", &[]);
        let score = score_release(
            "Yorushika",
            "幻燈",
            Some(2023),
            25,
            &[],
            0,
            &MbRelease {
                track_count: 25,
                year: Some(2023),
                ..release
            },
        );
        assert!(
            score.total > 0.95,
            "script-mismatched artist should not sink an otherwise perfect match: {}",
            score.total
        );
    }

    #[test]
    fn script_mismatch_does_not_tank_score() {
        // Local tags say "マエナラワナイ", MB returns the romanized
        // "Maenarawanai" — same album, different script. Jaro-Winkler scores
        // ~0 across scripts; without special handling this sinks the total
        // even when every other signal is a perfect match.
        let release = make_release("ATARASHII GAKKO!", "Maenarawanai", &[]);
        let score = score_release(
            "ATARASHII GAKKO!",
            "マエナラワナイ",
            Some(2018),
            10,
            &[],
            0,
            &MbRelease {
                track_count: 10,
                year: Some(2018),
                ..release
            },
        );
        assert!(
            score.total > 0.95,
            "script-mismatched title should not sink an otherwise perfect match: {}",
            score.total
        );
    }

    #[test]
    fn different_albums_same_script_still_penalized() {
        // Sanity: the script-agnostic treatment must only kick in across
        // scripts. Two Latin-script albums with different names must still
        // score the album factor normally.
        let release = make_release("Artist", "OK Computer", &[]);
        let score = score_release(
            "Artist", "Kid A", None, 0, &[], 0, &release,
        );
        assert!(
            score.album < 0.8,
            "same-script differing titles must still penalize: {}",
            score.album
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
