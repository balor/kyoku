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
    ///
    /// Falls back through progressively looser searches if the first attempt
    /// yields nothing, including resolving the artist to an MBID (via the
    /// artist-search endpoint, which matches aliases) and searching releases
    /// by `arid:` — this catches releases credited in a different script than
    /// what the file tags say (e.g. file says "HANABIE.", MB release credits
    /// "花冷え。", both aliases of the same artist).
    pub fn search_releases(
        &mut self,
        artist: &str,
        album: &str,
        limit: u32,
    ) -> Result<Vec<MbRelease>> {
        // Clean inputs for matching:
        // - Artist: strip trailing punctuation (HANABIE. → HANABIE, t.A.T.u → kept)
        // - Album: strip trailing parenthesized qualifiers like "(bonus tracks)"
        let clean_artist = strip_trailing_punct(artist);
        let clean_album = strip_parenthesized_suffix(album);

        // First attempt: artist + album (string match on credit name)
        let releases = self.run_search_artist(&clean_artist, Some(&clean_album), limit)?;
        if !releases.is_empty() {
            return Ok(releases);
        }

        // Resolve the artist to an MBID via the artist endpoint, which indexes
        // aliases (sort-name, romanizations, etc). Cache the None case too so
        // we don't re-resolve on every fallback step.
        let arid = if !clean_artist.trim().is_empty() {
            self.resolve_artist_mbid(&clean_artist)?
        } else {
            None
        };

        // Fallback 1: arid + album — catches releases whose credit uses a
        // different script/name than the file tags.
        if let Some(ref mbid) = arid {
            let releases = self.run_search_arid(mbid, Some(&clean_album), limit)?;
            if !releases.is_empty() {
                return Ok(releases);
            }
        }

        // Fallback 2: artist only (string match), in case album spelling differs
        if !clean_artist.trim().is_empty() {
            let releases = self.run_search_artist(&clean_artist, None, limit)?;
            if !releases.is_empty() {
                return Ok(releases);
            }
        }

        // Fallback 3: arid only (all releases by that artist, any credit)
        if let Some(ref mbid) = arid {
            let releases = self.run_search_arid(mbid, None, limit)?;
            if !releases.is_empty() {
                return Ok(releases);
            }
        }

        // Fallback 4: original artist (with punctuation) + album
        if clean_artist != artist {
            let releases = self.run_search_artist(artist, Some(&clean_album), limit)?;
            if !releases.is_empty() {
                return Ok(releases);
            }
        }

        Ok(Vec::new())
    }

    /// Resolve an artist name (or alias) to its MusicBrainz artist MBID.
    /// Returns None if no plausible match is found.
    fn resolve_artist_mbid(&mut self, name: &str) -> Result<Option<String>> {
        let query = format!("artist:({})", escape_lucene(name));
        let url = format!(
            "{}/artist/?query={}&fmt=json&limit=1",
            MB_BASE,
            urlencoding(&query),
        );
        let body = self.get_json_body(&url, "artist-search")?;
        let resp: MbArtistSearchResponse = serde_json::from_str(&body).map_err(|e| {
            crate::error::KyokuError::External(format!(
                "MB artist parse failed: {}: body={}",
                e,
                truncate_for_log(&body, 200),
            ))
        })?;

        // Require a reasonable MB score so we don't misroute to an unrelated
        // artist when the name is absent.
        Ok(resp
            .artists
            .into_iter()
            .find(|a| a.score.unwrap_or(0) >= 90)
            .map(|a| a.id))
    }

    fn run_search_artist(
        &mut self,
        artist: &str,
        album: Option<&str>,
        limit: u32,
    ) -> Result<Vec<MbRelease>> {
        let query = if let Some(album) = album {
            format!(
                "artist:({}) AND release:({})",
                escape_lucene(artist),
                escape_lucene(album),
            )
        } else {
            format!("artist:({})", escape_lucene(artist))
        };
        self.run_release_query(&query, limit)
    }

    fn run_search_arid(
        &mut self,
        arid: &str,
        album: Option<&str>,
        limit: u32,
    ) -> Result<Vec<MbRelease>> {
        let query = if let Some(album) = album {
            format!(
                "arid:{} AND release:({})",
                arid,
                escape_lucene(album),
            )
        } else {
            format!("arid:{}", arid)
        };
        self.run_release_query(&query, limit)
    }

    fn run_release_query(&mut self, query: &str, limit: u32) -> Result<Vec<MbRelease>> {
        let url = format!(
            "{}/release/?query={}&fmt=json&limit={}",
            MB_BASE,
            urlencoding(query),
            limit,
        );

        let body = self.get_json_body(&url, "search")?;
        let resp: MbSearchResponse = serde_json::from_str(&body).map_err(|e| {
            crate::error::KyokuError::External(format!(
                "MB parse failed: {}: body={}",
                e,
                truncate_for_log(&body, 200),
            ))
        })?;

        Ok(resp
            .releases
            .into_iter()
            .map(parse_search_release)
            .collect())
    }

    /// Fetch a specific release with full track listing.
    pub fn fetch_release(&mut self, mbid: &str) -> Result<MbRelease> {
        let url = format!(
            "{}/release/{}?inc=recordings+artists+labels&fmt=json",
            MB_BASE, mbid,
        );

        let body = self.get_json_body(&url, "fetch")?;
        let raw: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            crate::error::KyokuError::External(format!(
                "MB parse failed: {}: body={}",
                e,
                truncate_for_log(&body, 200),
            ))
        })?;

        Ok(parse_full_release(raw))
    }

    /// GET `url` and return the response body as text. Auto-retries once on
    /// transient failures (5xx server errors, network errors). Permanent
    /// failures (4xx, parse errors) are returned immediately so the caller
    /// can surface the real reason instead of a downstream parse failure.
    fn get_json_body(&mut self, url: &str, op: &str) -> Result<String> {
        match self.attempt_get(url, op) {
            Ok(body) => Ok(body),
            Err(AttemptError::Retryable(msg)) => {
                tracing::warn!("MB {} transient failure, retrying once: {}", op, msg);
                // Small backoff on top of the normal throttle spacing.
                std::thread::sleep(Duration::from_millis(750));
                match self.attempt_get(url, op) {
                    Ok(body) => Ok(body),
                    Err(AttemptError::Retryable(msg)) | Err(AttemptError::Permanent(msg)) => {
                        Err(crate::error::KyokuError::External(format!(
                            "MB {} failed after retry: {}",
                            op, msg
                        )))
                    }
                }
            }
            Err(AttemptError::Permanent(msg)) => Err(crate::error::KyokuError::External(
                format!("MB {} failed: {}", op, msg),
            )),
        }
    }

    /// One request attempt. Throttles first so the caller doesn't have to,
    /// and so the throttle applies both to the first attempt and the retry.
    fn attempt_get(&mut self, url: &str, op: &str) -> std::result::Result<String, AttemptError> {
        self.throttle();

        let resp = match self.client.get(url).send() {
            Ok(r) => r,
            Err(e) => {
                // Network-level errors (timeout, DNS, TLS, connection refused)
                // are generally transient — worth one retry.
                let _ = op;
                return Err(AttemptError::Retryable(error_chain(&e)));
            }
        };

        let status = resp.status();
        let body = match resp.text() {
            Ok(b) => b,
            Err(e) => return Err(AttemptError::Retryable(error_chain(&e))),
        };

        if !status.is_success() {
            // MB error responses are JSON like {"error": "...", "help": "..."}.
            // Extract the message if we can, otherwise include the raw body.
            let detail = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| truncate_for_log(&body, 200));
            let msg = format!("HTTP {}: {}", status, detail);
            // 5xx server errors (incl. 503 "busy") are transient; 4xx are not.
            if status.is_server_error() {
                return Err(AttemptError::Retryable(msg));
            } else {
                return Err(AttemptError::Permanent(msg));
            }
        }

        Ok(body)
    }
}

enum AttemptError {
    Retryable(String),
    Permanent(String),
}

fn truncate_for_log(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(max).collect();
        out.push('…');
        out
    }
}

// ── MB JSON response types ──────────────────────────────────────────

#[derive(Deserialize)]
struct MbSearchResponse {
    releases: Vec<MbSearchRelease>,
}

#[derive(Deserialize)]
struct MbArtistSearchResponse {
    artists: Vec<MbArtistSearchHit>,
}

#[derive(Deserialize)]
struct MbArtistSearchHit {
    id: String,
    score: Option<u8>,
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
    #[serde(rename = "release-group")]
    release_group: Option<MbSearchReleaseGroup>,
}

#[derive(Deserialize)]
struct MbSearchReleaseGroup {
    #[serde(rename = "first-release-date")]
    first_release_date: Option<String>,
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
        .or_else(|| {
            r.release_group
                .as_ref()?
                .first_release_date
                .as_deref()
        })
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
        .or_else(|| v["release-group"]["first-release-date"].as_str())
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

/// Strip trailing punctuation from an artist name.
///
/// Examples:
///   "HANABIE." → "HANABIE"
///   "BABYMETAL!" → "BABYMETAL"
///   "t.A.T.u." → "t.A.T.u"  (only trailing punctuation, internal dots kept)
///
/// We don't strip leading punctuation or internal dots — those can be part of
/// the actual band name.
fn strip_trailing_punct(s: &str) -> String {
    s.trim_end_matches(|c: char| {
        matches!(
            c,
            '.' | '!' | '?' | ',' | ';' | ':' | '。' | '！' | '？' | '、'
        )
    })
    .trim()
    .to_string()
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
/// Flatten an error and its `.source()` chain into a single colon-separated
/// string. reqwest's top-level Display is often just "error sending request",
/// with the actual cause (DNS, TLS, timeout) hiding one or two levels down.
fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

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
