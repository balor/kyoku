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
    pub country: f64,
    pub original: f64,
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
/// - Album similarity (0.15): Jaro-Winkler on the title string
/// - Track count (0.25): exact match is very strong signal — steepest penalty
///   for mismatch since it's the most objective metric we have from search.
/// - Year (0.15): exact/near release year match
/// - Country (0.06): press preference — worldwide/Europe and major
///   anglophone markets over random regional reissues. Deliberately small:
///   it only reorders candidates that are otherwise equivalent.
/// - Originality (0.08): candidate year vs. the earliest known year in its
///   release group. Well-known albums have dozens of equal-score pressings
///   on MB; this is what puts the original pressing ahead of the 2011
///   remaster when the local files carry no year of their own.
/// - Duration (0.05): total duration within tolerance
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
    let local_artist = local_artist.trim();
    let artist = if local_artist.is_empty()
        || is_various_artists_label(local_artist)
        || scripts_differ(local_artist, &candidate.artist)
    {
        // Various-Artists labels can't be string-compared against MB's
        // "Various Artists" credit in whatever language — exclude the
        // factor like an empty artist instead of penalising the correct
        // tribute album for the tagger's localisation choice.
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

    // Track count: ratio-based with an asymmetric curve.
    //
    // The two directions carry different priors:
    // - Candidate has *more* tracks than local: plausible (expanded edition,
    //   compilation, bonus-track version — the user just doesn't have them
    //   all). Linear `local/cand` — a 7-vs-11 split still scores ~0.64.
    // - Candidate has *fewer* tracks than local: can't be a correct match
    //   on its own — a 7-track local can't be a 1-track single. Square the
    //   `cand/local` ratio so small-candidate mismatches fall off a cliff
    //   (a 1-vs-7 case scores 0.02 here instead of 0.10 under the previous
    //   linear curve). This is the fix for the "7-track local gets 76%
    //   against a same-named single" case.
    //
    // 0.5 is still the "can't compare" sentinel when either side is zero
    // (loose tracks, missing search metadata).
    let track_count = if local_track_count == 0 || candidate.track_count == 0 {
        0.5
    } else if local_track_count == candidate.track_count {
        1.0
    } else if local_track_count < candidate.track_count {
        local_track_count as f64 / candidate.track_count as f64
    } else {
        let ratio = candidate.track_count as f64 / local_track_count as f64;
        ratio * ratio
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

    // Per-track title similarity. Excluded if data is unavailable (search
    // results don't include tracks).
    //
    // Matching is best-similarity, NOT positional: a partial album (local
    // tracks 3-8 of an 8-track release — a real Soulseek-rip case) would be
    // misaligned by an ordered zip, tanking an otherwise exact match. Each
    // local title greedily claims its best remaining MB track, so a subset
    // still scores ~1.0; completeness is penalised by the track_count
    // factor instead of paid for twice. Normalise over LOCAL track count,
    // so an all-garbage title set (nothing matches) scores near 0 and still
    // sinks wrong-album candidates.
    let tracks = if local_track_titles.is_empty() || candidate.tracks.is_empty() {
        None
    } else {
        let mut pairs: Vec<(usize, usize, f64)> = Vec::new();
        for (li, local) in local_track_titles.iter().enumerate() {
            for (mi, mb) in candidate.tracks.iter().enumerate() {
                let s = sim(local, &mb.title);
                if s >= 0.70 {
                    pairs.push((li, mi, s));
                }
            }
        }
        pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let mut claimed_local = vec![false; local_track_titles.len()];
        let mut claimed_mb = vec![false; candidate.tracks.len()];
        let mut sum = 0.0;
        for (li, mi, s) in pairs {
            if claimed_local[li] || claimed_mb[mi] {
                continue;
            }
            claimed_local[li] = true;
            claimed_mb[mi] = true;
            sum += s;
        }
        Some(sum / local_track_titles.len() as f64)
    };

    // Press-country preference. Excluded entirely when MB exposes no
    // country (digital releases sometimes don't) rather than penalised.
    let country = candidate.country.as_deref().map(country_preference_score);

    // Original-pressing preference: how close the candidate's year is to
    // the earliest known year in its release group. A 1976 original scores
    // 1.0; a 2011 remaster of the same album lands near 0.1.
    let original = match (candidate.year, candidate.group_min_year) {
        (Some(y), Some(min)) => Some(originality_score((y - min).max(0))),
        _ => None,
    };

    // Duration comparison. Excluded if data unavailable — and skipped
    // entirely for partial albums: local tracks 3-8 of an 8-track release
    // can never approach the full release's total duration, so the factor
    // would be a guaranteed ~0 punishing the (otherwise best) candidate.
    // Track-count mismatch is already priced in by the track_count factor.
    let duration = if local_total_duration_ms == 0
        || local_track_count == 0
        || candidate.track_count == 0
        || local_track_count != candidate.track_count
    {
        None
    } else {
        let mb_duration: u64 = candidate.tracks.iter().filter_map(|t| t.duration_ms).sum();
        if mb_duration == 0 {
            None
        } else {
            let diff = (local_total_duration_ms as f64 - mb_duration as f64).abs();
            let tolerance = (local_track_count.max(1) * 10_000) as f64;
            Some((1.0 - diff / tolerance).clamp(0.0, 1.0))
        }
    };

    // Weighted sum — only include factors that have real data.
    // When optional factors are unavailable, their weight is redistributed
    // proportionally among the factors that DO have data.
    let mut total_weight = 0.0f64;
    let mut weighted_sum = 0.0f64;

    // Always available
    let factors: &[(f64, f64)] = &[(api_score, 0.10), (track_count, 0.25)];
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
    if let Some(c) = country {
        weighted_sum += c * 0.06;
        total_weight += 0.06;
    }
    if let Some(o) = original {
        weighted_sum += o * 0.08;
        total_weight += 0.08;
    }

    let mut total = if total_weight > 0.0 {
        weighted_sum / total_weight
    } else {
        0.0
    };

    // Pseudo-releases are translations/transliterations, not the release the
    // user should normally import against. Keep them visible as a fallback,
    // but rank real releases from the same release group above them.
    if candidate.is_pseudo_release() {
        total *= 0.85;
    }

    // Bootlegs are rarely what the user ripped. Same keep-visible-but-
    // demote treatment as pseudo-releases, weaker since an unofficial
    // live/bootleg recording can occasionally be the correct target.
    if candidate
        .status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("Bootleg"))
    {
        total *= 0.92;
    }

    MatchScore {
        total,
        artist: artist.unwrap_or(0.0),
        album: album.unwrap_or(0.0),
        track_count,
        year: year.unwrap_or(0.0),
        duration: duration.unwrap_or(0.0),
        tracks: tracks.unwrap_or(0.0),
        country: country.unwrap_or(0.0),
        original: original.unwrap_or(0.0),
    }
}

/// Pressing preference by MB release country code. Worldwide (`XW`) and
/// pan-European (`XE`) digital/CD releases are what most libraries centre
/// on; major original markets follow. Not a quality judgement — pure
/// tie-breaking among otherwise indistinguishable pressings.
fn country_preference_score(code: &str) -> f64 {
    match code {
        "XW" => 1.0,
        "XE" => 0.95,
        "GB" | "US" => 0.90,
        "JP" => 0.85,
        "DE" | "FR" | "NL" | "SE" | "IT" | "ES" | "AU" | "CA" => 0.80,
        "XU" => 0.60,
        _ => 0.70,
    }
}

/// Originality decay by years after the release group's earliest known
/// year. The original pressing scores 1.0; reissues fade fast but never to
/// zero — a remaster can still be the right match when the local files
/// came from it (the `year` factor handles that case when tags know it).
fn originality_score(years_after_original: i32) -> f64 {
    match years_after_original {
        0 => 1.0,
        1 => 0.75,
        2 => 0.55,
        3 => 0.40,
        4..=5 => 0.25,
        6..=8 => 0.15,
        _ => 0.08,
    }
}

/// True for Various-Artists-style album_artist labels produced by real
/// taggers/rippers: "Various Artists", "VA"/"V.A.", "Artisti Vari"
/// (Italian), stray @-prefixed variants seen on Soulseek rips,
/// オムニバス... These never resolve usefully through MB artist
/// search — MB credits such releases to the special-purpose "Various
/// Artists" entity, which plain text search won't reach from a translated
/// label. Callers should treat such artists as "no artist constraint".
pub fn is_various_artists_label(s: &str) -> bool {
    if s.contains("オムニバス") || s.contains("ヴァリアス") {
        return true;
    }
    // Lowercase and drop anything that isn't a Latin letter — this folds
    // "@Artisti Vari", "V.A.", "V/A" etc. into comparable forms.
    let compact: String = s
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .flat_map(|c| c.to_lowercase())
        .collect();
    matches!(
        compact.as_str(),
        "variousartists" | "variousartist" | "various" | "va" | "artistivari" | "variartisti"
    )
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

/// True when `s` contains no letters from any non-Latin script. Pure digits,
/// punctuation, and whitespace count as Latin-safe (nothing to romanise).
///
/// Used by the MB name-preference resolver to skip alias lookups when the
/// canonical name is already in Latin script.
pub fn is_pure_latin(s: &str) -> bool {
    scripts_of(s) & !S_LATIN == 0
}

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
            || (0x20000..=0x2EBEF).contains(&u)
            || (0x30000..=0x3134F).contains(&u)
            || (0xF900..=0xFAFF).contains(&u)
        {
            set |= S_CJK; // CJK Unified Ideographs, Ext A-G, Compat
        } else if (0xAC00..=0xD7AF).contains(&u)
            || (0x1100..=0x11FF).contains(&u)
            || (0xA960..=0xA97F).contains(&u)
            || (0xD7B0..=0xD7FF).contains(&u)
        {
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

    fn make_release(artist: &str, title: &str, tracks: &[&str]) -> MbRelease {
        MbRelease {
            id: "test-id".to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            year: Some(1997),
            country: Some("GB".to_string()),
            label: None,
            status: None,
            track_count: tracks.len() as u32,
            medium_count: 1,
            release_group_id: None,
            tracks: tracks
                .iter()
                .enumerate()
                .map(|(i, t)| MbTrack {
                    disc: 1,
                    position: (i + 1) as u32,
                    title: t.to_string(),
                    artist: None,
                    duration_ms: Some(240_000),
                    recording_id: String::new(),
                })
                .collect(),
            api_score: 100,
            group_min_year: None,
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
            "Radiohead",
            "OK Computer",
            Some(1997),
            1,
            &["Airbag".to_string()],
            240_000,
            &release,
        );
        let score_wrong = score_release(
            "Radiohead",
            "OK Computer",
            Some(2020),
            1,
            &["Airbag".to_string()],
            240_000,
            &release,
        );
        assert!(
            score_correct.total > score_wrong.total,
            "correct year {} should beat wrong year {}",
            score_correct.total,
            score_wrong.total
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
        let score = score_release("Artist", "Kid A", None, 0, &[], 0, &release);
        assert!(
            score.album < 0.8,
            "same-script differing titles must still penalize: {}",
            score.album
        );
    }

    #[test]
    fn partial_album_subset_scores_high_via_aligned_titles() {
        // Real case: Soulseek rip of Selling England by the Pound missing
        // tracks 1-2 (local files are tracks 03-08). The tagged pressing
        // scored 81% — below trackless alternates — because titles were
        // compared positionally (Firth of Fifth vs Dancing with the
        // Moonlit Knight) and duration against the full release.
        let full: Vec<&str> = vec![
            "Dancing with the Moonlit Knight",
            "I Know What I Like (In Your Wardrobe)",
            "Firth of Fifth",
            "More Fool Me",
            "The Battle of Epping Forest",
            "After the Ordeal",
            "The Cinema Show",
            "Aisle of Plenty",
        ];
        let release = make_release("Genesis", "Selling England by the Pound", &full);
        let local_titles: Vec<String> = full[2..].iter().map(|s| s.to_string()).collect();

        let score = score_release(
            "Genesis",
            "Selling England by the Pound",
            Some(1973),
            6,
            &local_titles,
            // Incomplete album: total duration necessarily lacks the first
            // two tracks (~14min) — this used to zero the duration factor.
            2_487_811,
            &MbRelease {
                year: Some(1973),
                group_min_year: Some(1973),
                ..release
            },
        );

        assert!(
            score.tracks > 0.95,
            "subset titles must align to their real MB tracks: {}",
            score.tracks
        );
        assert!(
            score.total > 0.90,
            "partial but correct match should score like one: {}",
            score.total
        );
    }

    #[test]
    fn unrelated_titles_still_sink_the_tracks_factor() {
        // Greedy alignment must not rescue a wrong album: garbage titles
        // don't meet the pairing threshold, so the factor collapses.
        let release = make_release(
            "Genesis",
            "Selling England by the Pound",
            &["Dancing with the Moonlit Knight", "More Fool Me"],
        );
        let score = score_release(
            "Genesis",
            "Selling England by the Pound",
            Some(1973),
            2,
            &["xyzzy01".to_string(), "qkvjty556".to_string()],
            0,
            &release,
        );
        assert!(
            score.tracks < 0.2,
            "unrelated titles must not pair: {}",
            score.tracks
        );
    }

    #[test]
    fn duration_factor_applies_when_track_counts_agree() {
        // Equal counts: duration mismatch is a real signal again.
        let release = make_release("A", "B", &["t1", "t2"]); // 2 x 240s = 480s
        let score = score_release(
            "A",
            "B",
            Some(1997),
            2,
            &["t1".into(), "t2".into()],
            480_000,
            &release,
        );
        assert!(
            (score.duration - 1.0).abs() < 1e-9,
            "equal counts + equal duration should give 1.0: {}",
            score.duration
        );
    }

    #[test]
    fn duration_factor_skipped_for_partial_albums() {
        let release = make_release("A", "B", &["t1", "t2", "t3", "t4"]);
        let score = score_release(
            "A",
            "B",
            Some(1997),
            2,
            &["t3".into(), "t4".into()],
            240_000, // only the 2 local tracks' worth
            &release,
        );
        assert_eq!(
            score.duration, 0.0,
            "factor must be excluded (surfaced as 0.0 in MatchScore), not penalised"
        );
    }

    #[test]
    fn cjk_extension_b_counts_as_cjk_script() {
        let ext_b = "𠀀";
        assert!(!is_pure_latin(ext_b));
        assert!(scripts_differ(ext_b, "Latin Title"));
    }

    #[test]
    fn empty_local_artist_is_excluded_from_weighting() {
        let release = make_release("Known Artist", "Perfect Album", &[]);
        let score = score_release(
            "   ",
            "Perfect Album",
            Some(1997),
            12,
            &[],
            0,
            &MbRelease {
                track_count: 12,
                year: Some(1997),
                ..release
            },
        );
        assert!(
            score.total >= 0.85,
            "artist-less perfect album should still auto-match: {}",
            score.total
        );
    }

    #[test]
    fn is_pure_latin_basics() {
        assert!(is_pure_latin("Yorushika"));
        assert!(is_pure_latin("HANABIE."));
        assert!(is_pure_latin("Björk"));
        assert!(is_pure_latin("Year 2023"));
        assert!(is_pure_latin(""), "empty → no non-Latin letters");
        assert!(is_pure_latin("123"), "digits-only → no script bits");
        assert!(!is_pure_latin("ヨルシカ"));
        assert!(!is_pure_latin("花冷え。"));
        assert!(!is_pure_latin("幻燈"));
        assert!(!is_pure_latin("BUMP OF CHICKEN 結成"));
    }

    #[test]
    fn various_artists_labels_detected() {
        for s in [
            "Various Artists",
            "various artists",
            "Various",
            "VA",
            "V.A.",
            "V/A",
            "Artisti Vari",
            "@Artisti Vari", // real tag from an Italian Soulseek rip
            "オムニバス",
            "ヴァリアス・アーティスト",
        ] {
            assert!(is_various_artists_label(s), "{s:?} should be VA");
        }
        for s in ["Genesis", "Variety Show", "Vangelis", ""] {
            assert!(!is_various_artists_label(s), "{s:?} is not VA");
        }
    }

    #[test]
    fn various_artists_label_excluded_from_scoring() {
        // The tribute-album case: local album_artist is a localised VA
        // label, MB credit is "Various Artists". The artist factor must be
        // excluded, not scored near-zero — otherwise the *correct* tribute
        // release sinks below random pressings.
        let tribute = make_release(
            "Various Artists",
            "Return to the Dark Side of the Moon",
            &[],
        );
        let tribute = MbRelease {
            track_count: 10,
            year: Some(2006),
            group_min_year: Some(2006),
            ..tribute
        };
        let score = score_release(
            "@Artisti Vari",
            "Return To The Dark Side Of The Moon (A Tribute To Pink Floyd)",
            Some(2006),
            10,
            &[],
            0,
            &tribute,
        );
        assert!(
            score.total > 0.85,
            "VA-labeled correct album must score as a clean match: {}",
            score.total
        );
    }

    #[test]
    fn single_track_candidate_loses_to_multi_track_album() {
        // Real-world case: local has 7 tracks of "Let me battle" by 9Lana;
        // MB surfaces a same-named 1-track single alongside the 11-track
        // album. Artist, album title, and year all match for both — only
        // track count distinguishes them. The single must score decisively
        // below the album so the UI surfaces the right candidate.
        let single = make_release("9Lana", "Let me battle", &[]);
        let single = MbRelease {
            track_count: 1,
            year: Some(2024),
            ..single
        };
        let album = make_release("9Lana", "Let me battle", &[]);
        let album = MbRelease {
            track_count: 11,
            year: Some(2024),
            ..album
        };

        let single_score = score_release("9Lana", "Let me battle", Some(2024), 7, &[], 0, &single);
        let album_score = score_release("9Lana", "Let me battle", Some(2024), 7, &[], 0, &album);

        assert!(
            album_score.total > single_score.total + 0.15,
            "album (11 trk) should decisively beat single (1 trk): album={}, single={}",
            album_score.total,
            single_score.total,
        );
        assert!(
            single_score.total < 0.72,
            "1-track single vs 7-track local must score below 0.72: {}",
            single_score.total,
        );
    }

    #[test]
    fn normalized_album_title_matches() {
        let release = make_release("Artist", "MMXX Hypa Hypa Edition", &[]);
        let score = score_release(
            "Artist",
            "MMXX (Hypa Hypa edition)",
            None,
            0,
            &[],
            0,
            &release,
        );
        assert!(
            score.album > 0.9,
            "normalized titles should match well: {}",
            score.album
        );
    }

    /// Candidate factory for the pressing-preference tests below: same
    /// artist/title/tracks, varying year/country within one release group.
    fn make_pressing(
        artist: &str,
        title: &str,
        year: Option<i32>,
        country: Option<&str>,
        group_min_year: Option<i32>,
    ) -> MbRelease {
        MbRelease {
            year,
            country: country.map(String::from),
            group_min_year,
            ..make_release(artist, title, &[])
        }
    }

    fn queen_pressing(year: Option<i32>, country: Option<&str>, group_min: Option<i32>) -> MbRelease {
        MbRelease {
            track_count: 10,
            ..make_pressing("Queen", "A Day at the Races", year, country, group_min)
        }
    }

    fn beatles_pressing(
        year: Option<i32>,
        country: Option<&str>,
        group_min: Option<i32>,
    ) -> MbRelease {
        MbRelease {
            track_count: 17,
            ..make_pressing("The Beatles", "Abbey Road", year, country, group_min)
        }
    }

    #[test]
    fn original_pressing_beats_reissue_when_local_year_known() {
        // The "A Day at the Races" report: MB's top hits for this album are
        // all 1990s–2010s reissues with identical relevance scores; the 1976
        // original sits deep in the list. With a local year tag of 1976 the
        // original must outrank every reissue by a wide margin.
        let original = queen_pressing(Some(1976), Some("US"), Some(1976));
        let reissue_2011 = queen_pressing(Some(2011), Some("XE"), Some(1976));
        let reissue_1993 = queen_pressing(Some(1993), Some("IT"), Some(1976));

        let s_orig = score_release("Queen", "A Day at the Races", Some(1976), 10, &[], 0, &original);
        let s_r2011 = score_release("Queen", "A Day at the Races", Some(1976), 10, &[], 0, &reissue_2011);
        let s_r1993 = score_release("Queen", "A Day at the Races", Some(1976), 10, &[], 0, &reissue_1993);

        assert!(s_orig.total > s_r2011.total + 0.15, "orig={} reissue={}", s_orig.total, s_r2011.total);
        assert!(s_orig.total > s_r1993.total + 0.15, "orig={} reissue={}", s_orig.total, s_r1993.total);
        assert!(s_orig.total >= 0.85, "original should auto-match: {}", s_orig.total);
    }

    #[test]
    fn original_pressing_beats_reissue_even_without_local_year() {
        // Local files carry no year at all — originality must still push the
        // group-min-year pressing to the top so the default pick is sane.
        let original = beatles_pressing(Some(1969), Some("US"), Some(1969));
        let reissue = beatles_pressing(Some(2012), Some("XE"), Some(1969));

        let s_orig = score_release("The Beatles", "Abbey Road", None, 17, &[], 0, &original);
        let s_reissue = score_release("The Beatles", "Abbey Road", None, 17, &[], 0, &reissue);

        assert!(s_orig.total > s_reissue.total, "orig={} reissue={}", s_orig.total, s_reissue.total);
    }

    #[test]
    fn worldwide_or_major_market_beats_exotic_pressing_on_ties() {
        // All else equal (the MB score-100 tie swamp), XE should beat CN,
        // and both should stay within review range of each other — the
        // country factor is a nudge, not a verdict.
        let xe = beatles_pressing(Some(1987), Some("XE"), Some(1969));
        let cn = beatles_pressing(Some(1987), Some("CN"), Some(1969));

        let s_xe = score_release("The Beatles", "Abbey Road", None, 17, &[], 0, &xe);
        let s_cn = score_release("The Beatles", "Abbey Road", None, 17, &[], 0, &cn);

        assert!(s_xe.total > s_cn.total, "XE={} CN={}", s_xe.total, s_cn.total);
        assert!(
            s_xe.total - s_cn.total < 0.10,
            "country must be only a nudge: XE={} CN={}",
            s_xe.total,
            s_cn.total
        );
    }

    #[test]
    fn candidate_without_year_gets_no_originality_penalty() {
        // A pressing with no known date (common for digital XW releases)
        // must not sink below every dated reissue — the originality factor
        // is excluded (weight redistributed), not zeroed.
        let undated = beatles_pressing(None, Some("XW"), Some(1969));
        let score = score_release("The Beatles", "Abbey Road", None, 17, &[], 0, &undated);
        assert!(
            score.total > 0.85,
            "undated XW pressing should stay competitive: {}",
            score.total
        );
    }

    #[test]
    fn bootleg_is_demoted_below_official_pressing() {
        let bootleg = MbRelease {
            status: Some("Bootleg".to_string()),
            ..beatles_pressing(Some(1969), Some("XW"), Some(1969))
        };
        let official = beatles_pressing(Some(1969), Some("US"), Some(1969));

        let s_boot = score_release("The Beatles", "Abbey Road", None, 17, &[], 0, &bootleg);
        let s_off = score_release("The Beatles", "Abbey Road", None, 17, &[], 0, &official);

        assert!(
            s_off.total > s_boot.total,
            "official={} bootleg={}",
            s_off.total,
            s_boot.total
        );
    }
}
