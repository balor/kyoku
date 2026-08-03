//! Live MusicBrainz probe for the import-matching pipeline.
//!
//! **Manual test — ignored by default.** Hits the real MusicBrainz API
//! (rate-limited to ~1 req/s like the app) and prints what the import
//! wizard would show for the folder names that historically produced bad
//! candidates.
//!
//! Run with:
//!
//! ```sh
//! cargo test --test mb_live_probe -- --ignored --nocapture
//! ```
//!
//! Requires network access and a MusicBrainz that isn't busy; transient
//! failures fail the test with a clear message (retry after a minute).

use kyoku::config::settings::NameScriptPreference;
use kyoku::core::importer::parse_folder_hints;
use kyoku::external::matching;
use kyoku::external::musicbrainz::{MbClient, MbRelease};

/// One real-world folder + how many local tracks it holds. The track count
/// feeds the Album/EP type filter exactly like the wizard does.
struct Scenario {
    folder: &'static str,
    /// Artist as found in the file tags; `None` models untagged rips where
    /// everything has to come from folder-name hints.
    tag_artist: Option<&'static str>,
    local_track_count: u32,
    /// Always-true expectations that hold regardless of MB data drift.
    expect_substring: &'static str,
    /// When set, the #1 ranked candidate must be by this artist — guards
    /// against regressions where a random pressing/tribute outranks the
    /// canonical album.
    leader_artist: Option<&'static str>,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        // Real tags: album_artist is the localised-VA label "@Artisti
        // Vari" (which used to fuzzy-hit the artist "Shari Vari"),
        // year 2006, 10 tracks.
        folder: "(2006) Return To The Dark Side Of The Moon (A Tribute To Pink Floyd) [FLAC]",
        tag_artist: Some("@Artisti Vari"),
        local_track_count: 10,
        expect_substring: "Return to the Dark Side of the Moon",
        leader_artist: Some("Various Artists"),
    },
    Scenario {
        folder: "1968 Procol Harum - Shine On Brightly",
        tag_artist: None,
        local_track_count: 14,
        expect_substring: "Shine On Brightly",
        leader_artist: Some("Procol Harum"),
    },
    Scenario {
        folder: "A Day at the Races (1976)",
        tag_artist: None,
        local_track_count: 10,
        expect_substring: "A Day at the Races",
        leader_artist: Some("Queen"),
    },
    Scenario {
        // Tags carry the artist; the folder basename is the only title hint.
        folder: "Abbey road",
        tag_artist: Some("The Beatles"),
        local_track_count: 17,
        expect_substring: "Abbey Road",
        leader_artist: Some("The Beatles"),
    },
    Scenario {
        folder: "News of the World (remastered)",
        tag_artist: None,
        local_track_count: 11,
        expect_substring: "News of the World",
        leader_artist: Some("Queen"),
    },
];

fn year_of(r: &MbRelease) -> String {
    r.year.map(|y| y.to_string()).unwrap_or_else(|| "-".into())
}

#[test]
#[ignore = "hits the live MusicBrainz API; run manually"]
fn live_mb_probe_for_known_problem_folders() {
    let mut client = MbClient::new(1100, NameScriptPreference::Native);

    let mut failures = Vec::new();

    for s in SCENARIOS {
        let hints = parse_folder_hints(s.folder);
        let album = hints.title.clone().unwrap_or_else(|| s.folder.into());
        let artist = s
            .tag_artist
            .map(str::to_string)
            .or_else(|| hints.artist.clone())
            .unwrap_or_default();

        println!("\n=== {}", s.folder);
        println!(
            "    hints: artist={:?} title={:?} year={:?}",
            artist, album, hints.year
        );

        let releases = match client.search_releases(&artist, &album, s.local_track_count, 5) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{}: search failed: {}", s.folder, e));
                continue;
            }
        };
        assert!(
            !releases.is_empty(),
            "{}: search returned no candidates at all",
            s.folder
        );

        let mut scored: Vec<_> = releases
            .iter()
            .map(|r| {
                let score = matching::score_release(
                    &artist,
                    &album,
                    hints.year,
                    s.local_track_count,
                    &[],
                    0,
                    r,
                );
                (r, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total.partial_cmp(&a.1.total).unwrap_or(std::cmp::Ordering::Equal));

        for (r, score) in scored.iter().take(5) {
            println!(
                "    {:>5.1}%  {}  {:<2}  tc={:<2} rgmin={:?} [{}] {}",
                score.total * 100.0,
                year_of(r),
                r.country.as_deref().unwrap_or("--"),
                r.track_count,
                r.group_min_year,
                r.artist,
                r.title,
            );
        }

        // The right album must show up in the displayed top-5 at all, and —
        // for these canonical albums — the leader must share at least one
        // token with the expected title.
        let top5_ok = scored.iter().take(5).any(|(r, _)| {
            r.title
                .to_lowercase()
                .contains(&s.expect_substring.to_lowercase())
        });
        if !top5_ok {
            failures.push(format!(
                "{}: no top-5 candidate contains {:?}",
                s.folder, s.expect_substring
            ));
        }

        if let Some(want) = s.leader_artist {
            let leader_ok = scored
                .first()
                .is_some_and(|(r, _)| r.artist.eq_ignore_ascii_case(want));
            if !leader_ok {
                failures.push(format!(
                    "{}: leader is {:?}, expected artist {:?}",
                    s.folder,
                    scored.first().map(|(r, _)| r.artist.as_str()),
                    want
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "live probe failures:\n{}",
        failures.join("\n")
    );
}

/// Regression probe for the "matched albums with NULL year in the DB"
/// report: these five release MBIDs were matched during a real import but
/// landed in `albums` without a year. `fetch_release` must recover a year
/// for every one of them (release date, release-events, or
/// release-group first-release-date fallback).
#[test]
#[ignore = "hits the live MusicBrainz API; run manually"]
fn live_mb_year_backfill_for_previously_yearless_releases() {
    let cases: &[(&str, &str, i32)] = &[
        ("9081951b-8fa8-4e71-8b50-6dfe059c9b25", "Sticky Fingers", 1971),
        ("87815df0-bb54-4ca2-a5e8-a21bf9d662eb", "A Trick of the Tail", 1976),
        ("75927c23-91ee-4b25-8b80-fbcf6521ac73", "Dire Straits", 1978),
        ("61d644bb-8851-4d37-b85e-ad796cd31972", "Out of the Blue", 1977),
        ("bacbd0b1-a596-43ee-ace4-1d19664fcd3c", "[Led Zeppelin IV]", 1971),
    ];

    let mut client = MbClient::new(1100, NameScriptPreference::Native);
    let mut failures = Vec::new();

    for (mbid, title, want_year) in cases {
        match client.fetch_release(mbid) {
            Ok(r) => {
                println!(
                    "{}: year={:?} rgmin={:?} [{}] {}",
                    title, r.year, r.group_min_year, r.artist, r.title
                );
                match r.year {
                    Some(y) if y == *want_year => {}
                    other => failures.push(format!(
                        "{}: year={:?}, wanted Some({})",
                        title, other, want_year
                    )),
                }
            }
            Err(e) => failures.push(format!("{}: fetch failed: {}", title, e)),
        }
    }

    assert!(
        failures.is_empty(),
        "year backfill failures:\n{}",
        failures.join("\n")
    );
}