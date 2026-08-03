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
    local_track_count: u32,
    /// Always-true expectations that hold regardless of MB data drift.
    expect_substring: &'static str,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        folder: "(2006) Return To The Dark Side Of The Moon (A Tribute To Pink Floyd) [FLAC]",
        local_track_count: 13,
        expect_substring: "Return to the Dark Side of the Moon",
    },
    Scenario {
        folder: "1968 Procol Harum - Shine On Brightly",
        local_track_count: 14,
        expect_substring: "Shine On Brightly",
    },
    Scenario {
        folder: "A Day at the Races (1976)",
        local_track_count: 10,
        expect_substring: "A Day at the Races",
    },
    Scenario {
        folder: "Abbey road",
        local_track_count: 17,
        expect_substring: "Abbey Road",
    },
    Scenario {
        folder: "News of the World (remastered)",
        local_track_count: 11,
        expect_substring: "News of the World",
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
        let artist = hints.artist.clone().unwrap_or_default();

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
    }

    assert!(
        failures.is_empty(),
        "live probe failures:\n{}",
        failures.join("\n")
    );
}