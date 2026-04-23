use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::config::settings::NameScriptPreference;
use crate::error::Result;
use crate::external::name_preference::{AliasKind, MbAlias, pick_preferred_name};

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
    /// Release-group MBID, used to look up `first-release-date` when the
    /// release itself has no date exposed in the search response.
    pub release_group_id: Option<String>,
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
    name_script: NameScriptPreference,
    /// Per-artist alias cache keyed by MBID. Shared across releases in one
    /// session so a multi-release import of the same artist pays the
    /// `/artist/{mbid}?inc=aliases` cost only once.
    artist_alias_cache: HashMap<String, Vec<MbAlias>>,
}

impl MbClient {
    pub fn new(rate_limit_ms: u64, name_script: NameScriptPreference) -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to create HTTP client");

        Self {
            client,
            rate_limit: Duration::from_millis(rate_limit_ms),
            last_request: None,
            name_script,
            artist_alias_cache: HashMap::new(),
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
        local_track_count: u32,
        limit: u32,
    ) -> Result<Vec<MbRelease>> {
        // Clean inputs for matching:
        // - Artist: strip trailing punctuation (HANABIE. → HANABIE, t.A.T.u → kept)
        // - Album: strip trailing parenthesized qualifiers like "(bonus tracks)"
        let clean_artist = strip_trailing_punct(artist);
        let clean_album = strip_parenthesized_suffix(album);

        // Groups with several tracks can't plausibly match a single. MB's
        // own relevance ranking doesn't know that — a same-named single
        // often outranks the real album, pushing the album out of the
        // top-N. Restrict the initial passes to Album/EP releases so the
        // first thing we see is the right *kind* of release, not just the
        // highest-scoring string match.
        //
        // `None` means "no type filter" — small groups (1-3 tracks) might
        // legitimately *be* a single, so we leave them alone.
        let type_filter: Option<&str> = if local_track_count >= 4 {
            Some("primarytype:(Album OR EP)")
        } else {
            None
        };

        // First attempt: artist + album (string match on credit name)
        let releases =
            self.run_search_artist(&clean_artist, Some(&clean_album), limit, type_filter)?;
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
        // different script/name than the file tags. Keep the Album/EP filter
        // here too — same reasoning as the first pass.
        if let Some(ref mbid) = arid {
            let releases = self.run_search_arid(mbid, Some(&clean_album), limit, type_filter)?;
            if !releases.is_empty() {
                return Ok(releases);
            }
        }

        // From here on, filtering is a rescue operation — we've already
        // tried the precise matches. Drop the type filter so we don't
        // leave the user empty-handed when MB happens to classify the
        // right release as e.g. Other/Broadcast/Compilation.

        // Fallback 2: artist + album, no type filter (catches misclassified releases)
        if type_filter.is_some() {
            let releases =
                self.run_search_artist(&clean_artist, Some(&clean_album), limit, None)?;
            if !releases.is_empty() {
                return Ok(releases);
            }
            if let Some(ref mbid) = arid {
                let releases = self.run_search_arid(mbid, Some(&clean_album), limit, None)?;
                if !releases.is_empty() {
                    return Ok(releases);
                }
            }
        }

        // Fallback 3: artist only (string match), in case album spelling differs
        if !clean_artist.trim().is_empty() {
            let releases = self.run_search_artist(&clean_artist, None, limit, None)?;
            if !releases.is_empty() {
                return Ok(releases);
            }
        }

        // Fallback 4: arid only (all releases by that artist, any credit)
        if let Some(ref mbid) = arid {
            let releases = self.run_search_arid(mbid, None, limit, None)?;
            if !releases.is_empty() {
                return Ok(releases);
            }
        }

        // Fallback 5: original artist (with punctuation) + album
        if clean_artist != artist {
            let releases = self.run_search_artist(artist, Some(&clean_album), limit, None)?;
            if !releases.is_empty() {
                return Ok(releases);
            }
        }

        Ok(Vec::new())
    }

    /// Resolve an artist name (or alias) to its MusicBrainz artist MBID.
    /// Returns None if no plausible match is found.
    ///
    /// Note: uses an unprefixed query (no `artist:` field qualifier). With
    /// `artist:(X)` MB only matches the canonical `name`; an unprefixed
    /// query fans out across `name`, `sortname`, and `alias`, which is what
    /// lets e.g. "HANABIE" resolve to 花冷え。's MBID via its Latin alias.
    fn resolve_artist_mbid(&mut self, name: &str) -> Result<Option<String>> {
        let query = escape_lucene(name);
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
        type_filter: Option<&str>,
    ) -> Result<Vec<MbRelease>> {
        let mut query = if let Some(album) = album {
            format!(
                "artist:({}) AND release:({})",
                escape_lucene(artist),
                escape_lucene(album),
            )
        } else {
            format!("artist:({})", escape_lucene(artist))
        };
        if let Some(tf) = type_filter {
            query.push_str(" AND ");
            query.push_str(tf);
        }
        self.run_release_query(&query, limit)
    }

    fn run_search_arid(
        &mut self,
        arid: &str,
        album: Option<&str>,
        limit: u32,
        type_filter: Option<&str>,
    ) -> Result<Vec<MbRelease>> {
        let mut query = if let Some(album) = album {
            format!(
                "arid:{} AND release:({})",
                arid,
                escape_lucene(album),
            )
        } else {
            format!("arid:{}", arid)
        };
        if let Some(tf) = type_filter {
            query.push_str(" AND ");
            query.push_str(tf);
        }
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

        let mut releases: Vec<MbRelease> = resp
            .releases
            .into_iter()
            .map(parse_search_release)
            .collect();

        // Year-fallback #4: when the search response carries no date info
        // for a release (e.g. MB's Maenarawanai — neither `date`,
        // `release-events`, nor `release-group.first-release-date` appear
        // in search output, though the release page on MB shows them), fill
        // the gap by fetching `first-release-date` from the release group.
        // Only fires per candidate that's actually missing a year, so the
        // extra API traffic is bounded and only the slower import-review
        // path pays the cost.
        self.enrich_missing_years(&mut releases);

        Ok(releases)
    }

    fn enrich_missing_years(&mut self, releases: &mut [MbRelease]) {
        for r in releases.iter_mut() {
            if r.year.is_some() {
                continue;
            }
            let Some(rg_id) = r.release_group_id.clone() else {
                continue;
            };
            match self.fetch_release_group_first_year(&rg_id) {
                Ok(Some(y)) => r.year = Some(y),
                Ok(None) => {}
                Err(e) => tracing::debug!(
                    "MB release-group {} year lookup failed: {}",
                    rg_id,
                    e
                ),
            }
        }
    }

    /// Fetch a release-group's `first-release-date` and return its year.
    /// Used to fill in year when the release search response omits date
    /// fields. Cheap compared to a full release fetch — the JSON payload
    /// is tiny.
    fn fetch_release_group_first_year(&mut self, rg_id: &str) -> Result<Option<i32>> {
        let url = format!("{}/release-group/{}?fmt=json", MB_BASE, rg_id);
        let body = self.get_json_body(&url, "release-group")?;
        let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            crate::error::KyokuError::External(format!(
                "MB release-group parse failed: {}: body={}",
                e,
                truncate_for_log(&body, 200),
            ))
        })?;
        Ok(v["first-release-date"]
            .as_str()
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse::<i32>().ok()))
    }

    /// Fetch a specific release with full track listing.
    ///
    /// When `name_script = Latin`, applies the preferred-name resolver to
    /// the release title and artist-credit strings (including per-track
    /// artist credits when they differ from the release credit). Track
    /// titles are intentionally left alone — MB's recording-level alias
    /// coverage is too sparse for a meaningful preference. Release-level
    /// aliases come in via `inc=aliases`; per-artist aliases need a
    /// separate `/artist/{mbid}` lookup and are cached on the client.
    pub fn fetch_release(&mut self, mbid: &str) -> Result<MbRelease> {
        let url = format!(
            "{}/release/{}?inc=recordings+artists+labels+aliases&fmt=json",
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

        let parsed = parse_full_release(&raw);
        let mut release = parsed.release;

        if self.name_script == NameScriptPreference::Native {
            return Ok(release);
        }

        // Latin preference — resolve release title + release artist + track
        // artists against alias tiers. Only fetch per-artist aliases when
        // the canonical name actually warrants a lookup (avoids hitting
        // `/artist/{mbid}` for already-Latin credits).
        release.title = pick_preferred_name(
            &release.title,
            None,
            &parsed.title_aliases,
            self.name_script,
            AliasKind::Release,
        );

        let pref = self.name_script;

        if let Some(artist_mbid) = parsed.release_artist_mbid.as_deref() {
            let aliases = self.get_artist_aliases(artist_mbid, &release.artist)?;
            release.artist = pick_preferred_name(
                &release.artist,
                parsed.release_artist_sort.as_deref(),
                aliases,
                pref,
                AliasKind::Artist,
            );
        }

        for (idx, track) in release.tracks.iter_mut().enumerate() {
            let Some(raw_artist) = track.artist.clone() else {
                continue;
            };
            let mbid_opt = parsed
                .track_artist_mbids
                .get(idx)
                .and_then(|o| o.as_deref());
            let sort = parsed
                .track_artist_sorts
                .get(idx)
                .and_then(|o| o.as_deref());
            let Some(mbid) = mbid_opt else { continue };
            let aliases = self.get_artist_aliases(mbid, &raw_artist)?;
            track.artist = Some(pick_preferred_name(
                &raw_artist,
                sort,
                aliases,
                pref,
                AliasKind::Artist,
            ));
        }

        Ok(release)
    }

    /// Return cached artist aliases, fetching on miss. Avoids the network
    /// call entirely when the canonical name is already pure Latin — the
    /// resolver would short-circuit to `canonical` anyway, so the request
    /// would be wasted rate-limit budget.
    fn get_artist_aliases(&mut self, mbid: &str, canonical: &str) -> Result<&[MbAlias]> {
        if crate::external::matching::is_pure_latin(canonical) {
            // Still cache an empty slot so repeated lookups don't re-check.
            self.artist_alias_cache
                .entry(mbid.to_string())
                .or_default();
            return Ok(self
                .artist_alias_cache
                .get(mbid)
                .map(Vec::as_slice)
                .unwrap_or(&[]));
        }
        if !self.artist_alias_cache.contains_key(mbid) {
            let aliases = self.fetch_artist_aliases(mbid)?;
            self.artist_alias_cache.insert(mbid.to_string(), aliases);
        }
        Ok(self
            .artist_alias_cache
            .get(mbid)
            .map(Vec::as_slice)
            .unwrap_or(&[]))
    }

    /// Fetch the alias list for one artist MBID.
    fn fetch_artist_aliases(&mut self, mbid: &str) -> Result<Vec<MbAlias>> {
        let url = format!("{}/artist/{}?inc=aliases&fmt=json", MB_BASE, mbid);
        let body = self.get_json_body(&url, "artist-aliases")?;
        let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            crate::error::KyokuError::External(format!(
                "MB artist-aliases parse failed: {}: body={}",
                e,
                truncate_for_log(&body, 200),
            ))
        })?;
        Ok(parse_aliases(&v))
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
    #[serde(rename = "release-events")]
    release_events: Option<Vec<MbReleaseEvent>>,
}

#[derive(Deserialize)]
struct MbSearchReleaseGroup {
    id: Option<String>,
    #[serde(rename = "first-release-date")]
    first_release_date: Option<String>,
}

#[derive(Deserialize)]
struct MbReleaseEvent {
    date: Option<String>,
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

    // Year fallback chain (search responses have inconsistent date coverage):
    //   1. `date` on the release itself
    //   2. `release-group.first-release-date` (rarely in search responses)
    //   3. earliest `release-events[].date` (sometimes missing here too)
    // If all three are absent, year is filled in later by a release-group
    // lookup (see `enrich_missing_years`).
    let rg_first = r
        .release_group
        .as_ref()
        .and_then(|rg| rg.first_release_date.as_deref());
    let earliest_event = r
        .release_events
        .as_ref()
        .and_then(|events| events.iter().filter_map(|e| e.date.as_deref()).min());
    let year = r
        .date
        .as_deref()
        .or(rg_first)
        .or(earliest_event)
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());

    let release_group_id = r.release_group.as_ref().and_then(|rg| rg.id.clone());

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
        release_group_id,
    }
}

/// Full-release parse result plus the auxiliary data the caller needs to
/// apply the script-preference resolver. `release` is the naive MB payload
/// with canonical names in place; fields below carry the alias payload and
/// per-credit MBIDs used to fetch per-artist alias lists on demand.
struct ParsedFullRelease {
    release: MbRelease,
    title_aliases: Vec<MbAlias>,
    release_artist_mbid: Option<String>,
    release_artist_sort: Option<String>,
    /// Same index as `release.tracks`. MBID of the track's first artist
    /// credit when present; `None` when MB didn't expose one.
    track_artist_mbids: Vec<Option<String>>,
    /// Parallel vec with `sort-name` per track artist credit.
    track_artist_sorts: Vec<Option<String>>,
}

fn parse_full_release(v: &serde_json::Value) -> ParsedFullRelease {
    let id = v["id"].as_str().unwrap_or("").to_string();
    let title = v["title"].as_str().unwrap_or("").to_string();

    let first_credit = v["artist-credit"].as_array().and_then(|ac| ac.first());
    let artist = first_credit
        .and_then(|c| {
            c["name"]
                .as_str()
                .or_else(|| c["artist"]["name"].as_str())
        })
        .unwrap_or("")
        .to_string();
    let release_artist_mbid = first_credit
        .and_then(|c| c["artist"]["id"].as_str())
        .map(|s| s.to_string());
    let release_artist_sort = first_credit
        .and_then(|c| c["artist"]["sort-name"].as_str())
        .map(|s| s.to_string());

    // Same year fallback chain as parse_search_release — direct release
    // lookups populate more fields than search but can still be missing
    // the top-level date (e.g. digital-only releases), so check
    // release-group and release-events as well.
    let earliest_event_date = v["release-events"]
        .as_array()
        .and_then(|events| events.iter().filter_map(|e| e["date"].as_str()).min());
    let year = v["date"]
        .as_str()
        .or_else(|| v["release-group"]["first-release-date"].as_str())
        .or(earliest_event_date)
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());

    let release_group_id = v["release-group"]["id"]
        .as_str()
        .map(|s| s.to_string());

    let country = v["country"].as_str().map(|s| s.to_string());

    let label = v["label-info"]
        .as_array()
        .and_then(|li| li.first())
        .and_then(|l| l["label"]["name"].as_str())
        .map(|s| s.to_string());

    let mut tracks = Vec::new();
    let mut track_artist_mbids: Vec<Option<String>> = Vec::new();
    let mut track_artist_sorts: Vec<Option<String>> = Vec::new();
    let mut track_count = 0u32;

    if let Some(media) = v["media"].as_array() {
        for medium in media {
            if let Some(track_list) = medium["tracks"].as_array() {
                for t in track_list {
                    track_count += 1;
                    let position = t["position"].as_u64().unwrap_or(0) as u32;
                    let t_title = t["title"].as_str().unwrap_or("").to_string();
                    let first_tc = t["artist-credit"].as_array().and_then(|ac| ac.first());
                    let t_artist = first_tc
                        .and_then(|c| c["name"].as_str().or(c["artist"]["name"].as_str()))
                        .map(|s| s.to_string());
                    let t_artist_mbid = first_tc
                        .and_then(|c| c["artist"]["id"].as_str())
                        .map(|s| s.to_string());
                    let t_artist_sort = first_tc
                        .and_then(|c| c["artist"]["sort-name"].as_str())
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
                    track_artist_mbids.push(t_artist_mbid);
                    track_artist_sorts.push(t_artist_sort);
                }
            }
        }
    }

    let title_aliases = parse_aliases(v);

    ParsedFullRelease {
        release: MbRelease {
            id,
            title,
            artist,
            year,
            country,
            label,
            track_count,
            tracks,
            api_score: 100,
            release_group_id,
        },
        title_aliases,
        release_artist_mbid,
        release_artist_sort,
        track_artist_mbids,
        track_artist_sorts,
    }
}

/// Pull the `aliases` array off any MB JSON object (release, artist, …) and
/// deserialise it into `MbAlias` values. Missing/null field yields an empty
/// vec — MB returns `aliases: []` for entities with no alternates anyway,
/// so callers don't need to distinguish.
fn parse_aliases(v: &serde_json::Value) -> Vec<MbAlias> {
    v["aliases"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| serde_json::from_value::<MbAlias>(a.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
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
