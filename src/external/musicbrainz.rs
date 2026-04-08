use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::Result;

const USER_AGENT: &str =
    concat!("kyoku/", env!("CARGO_PKG_VERSION"), " (https://github.com/kyoku-project/kyoku)");
const MB_BASE: &str = "https://musicbrainz.org/ws/2";

#[derive(Debug, Clone)]
pub struct MbRelease {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: Option<i32>,
    pub country: Option<String>,
    pub label: Option<String>,
    pub track_count: u32,
    pub tracks: Vec<MbTrack>,
    pub api_score: u8,
}

#[derive(Debug, Clone)]
pub struct MbTrack {
    pub position: u32,
    pub title: String,
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
    pub recording_id: String,
}

pub struct MbClient {
    client: reqwest::blocking::Client,
    rate_limit: Duration,
    last_request: Option<Instant>,
}

impl MbClient {
    pub fn new(rate_limit_ms: u64) -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to create HTTP client");

        Self {
            client,
            rate_limit: Duration::from_millis(rate_limit_ms),
            last_request: None,
        }
    }

    fn throttle(&mut self) {
        if let Some(last) = self.last_request {
            let elapsed = last.elapsed();
            if elapsed < self.rate_limit {
                std::thread::sleep(self.rate_limit - elapsed);
            }
        }
        self.last_request = Some(Instant::now());
    }

    /// Search for releases matching artist + album name.
    /// Returns up to `limit` results ordered by MB relevance score.
    pub fn search_releases(
        &mut self,
        artist: &str,
        album: &str,
        limit: u32,
    ) -> Result<Vec<MbRelease>> {
        self.throttle();

        // Clean up the album title for better MB matching:
        // - Strip trailing parenthesized qualifiers like "(bonus tracks version)",
        //   "(deluxe edition)", "(remastered)" — MB stores these in the
        //   disambiguation field, not the title.
        // - Trim whitespace left behind.
        let clean_album = strip_parenthesized_suffix(album);

        // Use bare terms (not quoted phrases) for a fuzzier search.
        // Quoting the whole title fails when file tags differ slightly from MB.
        let query = format!(
            "artist:({}) AND release:({})",
            escape_lucene(artist),
            escape_lucene(&clean_album),
        );

        let url = format!(
            "{}/release/?query={}&fmt=json&limit={}",
            MB_BASE,
            urlencoding(&query),
            limit,
        );

        let resp: MbSearchResponse = self
            .client
            .get(&url)
            .send()
            .map_err(|e| crate::error::KyokuError::Config(format!("MB search failed: {}", e)))?
            .json()
            .map_err(|e| crate::error::KyokuError::Config(format!("MB parse failed: {}", e)))?;

        let releases = resp
            .releases
            .into_iter()
            .map(|r| parse_search_release(r))
            .collect();

        Ok(releases)
    }

    /// Fetch a specific release with full track listing.
    pub fn fetch_release(&mut self, mbid: &str) -> Result<MbRelease> {
        self.throttle();

        let url = format!(
            "{}/release/{}?inc=recordings+artists+labels&fmt=json",
            MB_BASE, mbid,
        );

        let raw: serde_json::Value = self
            .client
            .get(&url)
            .send()
            .map_err(|e| crate::error::KyokuError::Config(format!("MB fetch failed: {}", e)))?
            .json()
            .map_err(|e| crate::error::KyokuError::Config(format!("MB parse failed: {}", e)))?;

        Ok(parse_full_release(raw))
    }
}

// ── MB JSON response types ──────────────────────────────────────────

#[derive(Deserialize)]
struct MbSearchResponse {
    releases: Vec<MbSearchRelease>,
}

#[derive(Deserialize)]
struct MbSearchRelease {
    id: String,
    title: String,
    score: Option<u8>,
    date: Option<String>,
    country: Option<String>,
    #[serde(rename = "track-count")]
    track_count: Option<u32>,
    #[serde(rename = "artist-credit")]
    artist_credit: Option<Vec<MbArtistCredit>>,
    #[serde(rename = "label-info")]
    label_info: Option<Vec<MbLabelInfo>>,
}

#[derive(Deserialize)]
struct MbArtistCredit {
    name: Option<String>,
    artist: Option<MbArtist>,
}

#[derive(Deserialize)]
struct MbArtist {
    name: Option<String>,
}

#[derive(Deserialize)]
struct MbLabelInfo {
    label: Option<MbLabel>,
}

#[derive(Deserialize)]
struct MbLabel {
    name: Option<String>,
}

fn parse_search_release(r: MbSearchRelease) -> MbRelease {
    let artist = r
        .artist_credit
        .as_ref()
        .and_then(|ac| ac.first())
        .and_then(|c| c.name.clone().or_else(|| c.artist.as_ref()?.name.clone()))
        .unwrap_or_default();

    let label = r
        .label_info
        .as_ref()
        .and_then(|li| li.first())
        .and_then(|l| l.label.as_ref())
        .and_then(|l| l.name.clone());

    let year = r
        .date
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());

    MbRelease {
        id: r.id,
        title: r.title,
        artist,
        year,
        country: r.country,
        label,
        track_count: r.track_count.unwrap_or(0),
        tracks: Vec::new(), // Search results don't include tracks
        api_score: r.score.unwrap_or(0),
    }
}

fn parse_full_release(v: serde_json::Value) -> MbRelease {
    let id = v["id"].as_str().unwrap_or("").to_string();
    let title = v["title"].as_str().unwrap_or("").to_string();

    let artist = v["artist-credit"]
        .as_array()
        .and_then(|ac| ac.first())
        .and_then(|c| {
            c["name"]
                .as_str()
                .or_else(|| c["artist"]["name"].as_str())
        })
        .unwrap_or("")
        .to_string();

    let year = v["date"]
        .as_str()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());

    let country = v["country"].as_str().map(|s| s.to_string());

    let label = v["label-info"]
        .as_array()
        .and_then(|li| li.first())
        .and_then(|l| l["label"]["name"].as_str())
        .map(|s| s.to_string());

    let mut tracks = Vec::new();
    let mut track_count = 0u32;

    if let Some(media) = v["media"].as_array() {
        for medium in media {
            if let Some(track_list) = medium["tracks"].as_array() {
                for t in track_list {
                    track_count += 1;
                    let position = t["position"].as_u64().unwrap_or(0) as u32;
                    let t_title = t["title"].as_str().unwrap_or("").to_string();
                    let t_artist = t["artist-credit"]
                        .as_array()
                        .and_then(|ac| ac.first())
                        .and_then(|c| c["name"].as_str().or(c["artist"]["name"].as_str()))
                        .map(|s| s.to_string());
                    let duration_ms = t["length"].as_u64();
                    let recording_id = t["recording"]["id"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();

                    tracks.push(MbTrack {
                        position,
                        title: t_title,
                        artist: t_artist,
                        duration_ms,
                        recording_id,
                    });
                }
            }
        }
    }

    MbRelease {
        id,
        title,
        artist,
        year,
        country,
        label,
        track_count,
        tracks,
        api_score: 100,
    }
}

/// Minimal URL encoding for the query parameter.
fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '"' => "%22".to_string(),
            ':' => "%3A".to_string(),
            '&' => "%26".to_string(),
            '+' => "%2B".to_string(),
            '?' => "%3F".to_string(),
            '#' => "%23".to_string(),
            _ if c.is_ascii_alphanumeric() || "-._~()!*'".contains(c) => c.to_string(),
            _ => {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf);
                buf[..c.len_utf8()]
                    .iter()
                    .map(|b| format!("%{:02X}", b))
                    .collect()
            }
        })
        .collect()
}

/// Strip trailing parenthesized text from an album title.
///
/// File tags often contain qualifiers like "(Deluxe Edition)", "(Bonus Tracks
/// Version)", "(Remastered 2024)" that MusicBrainz stores in the
/// `disambiguation` field instead of the title. Keeping them in the search
/// query causes misses.
///
/// Examples:
///   "Rehab (bonus tracks version)" → "Rehab"
///   "OK Computer (Deluxe Edition)" → "OK Computer"
///   "Kid A"                        → "Kid A"  (no change)
fn strip_parenthesized_suffix(s: &str) -> String {
    // Strip everything from the last '(' to the end, if the '(' is preceded
    // by at least one non-paren character (i.e. isn't the entire title).
    if let Some(idx) = s.rfind('(') {
        let before = s[..idx].trim_end();
        if !before.is_empty() {
            return before.to_string();
        }
    }
    s.to_string()
}

/// Escape special Lucene characters in a search query value.
fn escape_lucene(s: &str) -> String {
    let special = [
        '+', '-', '&', '|', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':',
        '\\', '/',
    ];
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if special.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
