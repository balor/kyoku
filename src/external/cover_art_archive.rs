//! Cover Art Archive (CAA) HTTP client.
//!
//! CAA is the MusicBrainz-affiliated cover-art service. The public endpoint
//! `/release/{mbid}/front-500` returns a 500px-wide front cover (redirecting
//! to the Internet Archive CDN), which is plenty for a terminal preview and
//! keeps payloads small enough that decode is cheap.
//!
//! The client shape mirrors [`crate::external::musicbrainz::MbClient`] — a
//! blocking reqwest client with its own throttle, one retry on transient
//! failures, permanent errors (4xx other than 404) surfaced immediately.
//! 404 is special-cased: no cover is a normal outcome, not an error, so the
//! fetch returns `Ok(None)` rather than burning the retry and surfacing an
//! "External" error string.

use std::time::Duration;

use crate::config::settings::CoverArtSize;
use crate::error::{KyokuError, Result};
use crate::external::http::{self, AttemptError};
const CAA_BASE: &str = "https://coverartarchive.org";

/// A fetched cover image. `extension` is derived from the response's
/// Content-Type so callers don't have to re-sniff the bytes.
#[derive(Debug, Clone)]
pub struct CoverImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

pub struct CaaClient {
    client: reqwest::blocking::Client,
    throttle: http::Throttle,
}

impl CaaClient {
    pub fn new(rate_limit_ms: u64) -> Self {
        let client = http::blocking_client(Duration::from_secs(20));

        Self {
            client,
            throttle: http::Throttle::new(rate_limit_ms),
        }
    }

    /// Fetch the front cover for a release MBID at the requested size.
    ///
    /// Returns `Ok(None)` when the release has no cover (404). Returns
    /// `Err` for any other failure after one retry on transient errors.
    ///
    /// `CoverArtSize::Original` falls back to 1200 → 500 if the original
    /// upload isn't archived (CAA returns 404 for `/front` on releases
    /// where only thumbnails were generated). The fixed-size endpoints
    /// don't fall back: a 404 there means the release has no cover at
    /// all, which we want to surface as `Ok(None)`, not silently retry.
    pub fn fetch_front(
        &mut self,
        release_mbid: &str,
        size: CoverArtSize,
    ) -> Result<Option<CoverImage>> {
        // For Original we try in descending order until something hits.
        // Fixed sizes are a single attempt — a 404 there is authoritative.
        let attempts: &[CoverArtSize] = match size {
            CoverArtSize::Original => &[
                CoverArtSize::Original,
                CoverArtSize::Px1200,
                CoverArtSize::Px500,
            ],
            other => match other {
                CoverArtSize::Px250 => &[CoverArtSize::Px250],
                CoverArtSize::Px500 => &[CoverArtSize::Px500],
                CoverArtSize::Px1200 => &[CoverArtSize::Px1200],
                CoverArtSize::Original => unreachable!(),
            },
        };

        for s in attempts {
            let url = format!(
                "{}/release/{}/front{}",
                CAA_BASE,
                release_mbid,
                s.url_suffix()
            );
            match self.attempt(&url) {
                Ok(Some(img)) => return Ok(Some(img)),
                Ok(None) => {
                    // 404 on the original is "no original archived" — keep
                    // walking the fallback chain. 404 on a fixed size is
                    // "no cover for this release at all" — return None.
                    if attempts.len() == 1 {
                        return Ok(None);
                    }
                    continue;
                }
                Err(AttemptError::Retryable(msg)) => {
                    tracing::warn!(
                        "CAA front{} for {} transient failure, retrying once: {}",
                        s.url_suffix(),
                        release_mbid,
                        msg
                    );
                    std::thread::sleep(Duration::from_millis(750));
                    match self.attempt(&url) {
                        Ok(Some(img)) => return Ok(Some(img)),
                        Ok(None) => {
                            if attempts.len() == 1 {
                                return Ok(None);
                            }
                            continue;
                        }
                        Err(AttemptError::Retryable(msg)) | Err(AttemptError::Permanent(msg)) => {
                            return Err(KyokuError::External(format!(
                                "CAA fetch for {} failed after retry: {}",
                                release_mbid, msg
                            )));
                        }
                    }
                }
                Err(AttemptError::Permanent(msg)) => {
                    return Err(KyokuError::External(format!(
                        "CAA fetch for {} failed: {}",
                        release_mbid, msg
                    )));
                }
            }
        }

        // Original-chain exhausted without a hit → no cover at any size.
        Ok(None)
    }

    fn attempt(&mut self, url: &str) -> std::result::Result<Option<CoverImage>, AttemptError> {
        self.throttle.wait();

        let resp = match self.client.get(url).send() {
            Ok(r) => r,
            Err(e) => return Err(AttemptError::Retryable(http::error_chain(&e))),
        };

        let status = resp.status();

        // 404 → release has no cover art. Normal outcome, not an error.
        if status.as_u16() == 404 {
            return Ok(None);
        }

        if !status.is_success() {
            let msg = format!("HTTP {}", status);
            if status.is_server_error() {
                return Err(AttemptError::Retryable(msg));
            } else {
                return Err(AttemptError::Permanent(msg));
            }
        }

        // Pull content-type BEFORE consuming the body; `bytes()` moves resp.
        let extension = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(extension_for_mime)
            .unwrap_or("jpg");

        let bytes = match resp.bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Err(AttemptError::Retryable(http::error_chain(&e))),
        };

        Ok(Some(CoverImage { bytes, extension }))
    }
}

/// Map CAA-served MIMEs to a file extension usable as `cover.<ext>`.
/// Conservative default of `jpg` — CAA serves JPEG for the overwhelming
/// majority of covers, and the organizer already treats `cover.jpg` as the
/// canonical name in the sibling-cover detection list.
fn extension_for_mime(mime: &str) -> &'static str {
    let lower = mime.to_ascii_lowercase();
    if lower.contains("png") {
        "png"
    } else if lower.contains("webp") {
        "webp"
    } else {
        // image/jpeg, image/jpg, unknown → default to jpg
        "jpg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_maps_common_mimes() {
        assert_eq!(extension_for_mime("image/jpeg"), "jpg");
        assert_eq!(extension_for_mime("image/jpg"), "jpg");
        assert_eq!(extension_for_mime("image/png"), "png");
        assert_eq!(extension_for_mime("IMAGE/PNG"), "png");
        assert_eq!(extension_for_mime("image/webp"), "webp");
        assert_eq!(extension_for_mime("application/octet-stream"), "jpg");
        assert_eq!(extension_for_mime(""), "jpg");
    }
}
