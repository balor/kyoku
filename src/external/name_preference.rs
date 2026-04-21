//! Script-preference resolver for MusicBrainz-derived names.
//!
//! MB can return either the canonical credit name (often native script, e.g.
//! `ヨルシカ`) or Latin-script aliases (`Yorushika`). The user's
//! `[musicbrainz] name_script` setting decides which variant is written to
//! DB, tags, and the on-disk tree.
//!
//! This module owns the selection logic. It is deliberately free of MB API
//! calls and network state — callers are responsible for fetching aliases
//! and passing them in; we just pick.
//!
//! Scope (per plan): artist + album-title names only. Track titles ride
//! through unchanged because MB's recording-level alias coverage is too
//! sparse for a meaningful preference.

use serde::Deserialize;

use crate::config::settings::NameScriptPreference;
use crate::external::matching::is_pure_latin;

/// One alternate-name entry returned by MB's `inc=aliases` endpoints. Mirrors
/// the subset of fields we care about across artist, release, and
/// release-group alias responses.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MbAlias {
    pub name: String,
    /// BCP-47-ish locale tag. `Some("en")`, `Some("ja")`, or `None`.
    #[serde(default)]
    pub locale: Option<String>,
    /// Alias category — "Artist name", "Release name", "Search hint", …
    #[serde(rename = "type", default)]
    pub type_: Option<String>,
    /// MB flags the most authoritative alias per (locale, type) tuple with
    /// `primary: true`. When absent in JSON, treat as false.
    #[serde(default)]
    pub primary: bool,
}

/// Which alias `type` string is considered on-topic for this call site.
/// MB uses distinct strings per entity kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasKind {
    Artist,
    Release,
}

impl AliasKind {
    fn type_label(self) -> &'static str {
        match self {
            AliasKind::Artist => "Artist name",
            AliasKind::Release => "Release name",
        }
    }
}

/// Choose the preferred rendering of a name given MB aliases and the user's
/// script preference.
///
/// For [`NameScriptPreference::Native`] the canonical name is always
/// returned — no alias lookup matters.
///
/// For [`NameScriptPreference::Latin`] the resolver walks:
/// 1. Canonical is already pure Latin → keep it.
/// 2. Aliases matching `kind.type_label()`, preferring locale-`en` + primary
///    + Latin, then locale-`en` + Latin, then any Latin alias.
/// 3. `sort_name` (typically Latinised per MB convention) if Latin.
/// 4. Canonical, as the documented fallback.
pub fn pick_preferred_name(
    canonical: &str,
    sort_name: Option<&str>,
    aliases: &[MbAlias],
    pref: NameScriptPreference,
    kind: AliasKind,
) -> String {
    if pref == NameScriptPreference::Native {
        return canonical.to_string();
    }

    // Latin preference below.
    if is_pure_latin(canonical) {
        return canonical.to_string();
    }

    let typed: Vec<&MbAlias> = aliases
        .iter()
        .filter(|a| {
            a.type_
                .as_deref()
                .map(|t| t == kind.type_label())
                .unwrap_or(true) // untyped aliases are usable (MB often omits type)
        })
        .collect();

    let is_en = |a: &&MbAlias| {
        a.locale
            .as_deref()
            .map(|l| l.starts_with("en"))
            .unwrap_or(false)
    };

    // Tier 1: en + primary + Latin
    if let Some(hit) = typed
        .iter()
        .copied()
        .find(|a| is_en(a) && a.primary && is_pure_latin(&a.name))
    {
        return hit.name.clone();
    }
    // Tier 2: en + Latin (primary flag is informational)
    if let Some(hit) = typed
        .iter()
        .copied()
        .find(|a| is_en(a) && is_pure_latin(&a.name))
    {
        return hit.name.clone();
    }
    // Tier 3: any Latin alias (no locale guarantee, but script matches)
    if let Some(hit) = typed.iter().copied().find(|a| is_pure_latin(&a.name)) {
        return hit.name.clone();
    }
    // Tier 4: sort-name — MB convention is Latinised for non-Latin artists.
    if let Some(sn) = sort_name
        && is_pure_latin(sn)
    {
        return sn.to_string();
    }

    canonical.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(name: &str, locale: Option<&str>, primary: bool, type_: Option<&str>) -> MbAlias {
        MbAlias {
            name: name.to_string(),
            locale: locale.map(String::from),
            type_: type_.map(String::from),
            primary,
        }
    }

    #[test]
    fn native_preserves_canonical_regardless_of_aliases() {
        let aliases = vec![alias("Yorushika", Some("en"), true, Some("Artist name"))];
        let got = pick_preferred_name(
            "ヨルシカ",
            Some("Yorushika"),
            &aliases,
            NameScriptPreference::Native,
            AliasKind::Artist,
        );
        assert_eq!(got, "ヨルシカ");
    }

    #[test]
    fn latin_picks_en_primary_artist_alias() {
        let aliases = vec![
            alias("ヨルシカ", Some("ja"), true, Some("Artist name")),
            alias("YORUSHIKA", None, false, None),
            alias("Yorushika", Some("en"), true, Some("Artist name")),
        ];
        let got = pick_preferred_name(
            "ヨルシカ",
            Some("Yorushika"),
            &aliases,
            NameScriptPreference::Latin,
            AliasKind::Artist,
        );
        assert_eq!(got, "Yorushika");
    }

    #[test]
    fn latin_keeps_canonical_when_already_latin() {
        let got = pick_preferred_name(
            "HANABIE.",
            None,
            &[],
            NameScriptPreference::Latin,
            AliasKind::Artist,
        );
        assert_eq!(got, "HANABIE.");
    }

    #[test]
    fn latin_falls_back_to_sort_name_when_no_alias() {
        let got = pick_preferred_name(
            "ヨルシカ",
            Some("Yorushika"),
            &[],
            NameScriptPreference::Latin,
            AliasKind::Artist,
        );
        assert_eq!(got, "Yorushika");
    }

    #[test]
    fn latin_falls_back_to_canonical_when_nothing_matches() {
        // Non-Latin canonical, no aliases, non-Latin sort name → keep canonical.
        let got = pick_preferred_name(
            "幻燈",
            Some("幻燈"),
            &[],
            NameScriptPreference::Latin,
            AliasKind::Release,
        );
        assert_eq!(got, "幻燈");
    }

    #[test]
    fn latin_picks_release_alias_of_matching_type() {
        // Same alias list but mixed kinds — Artist-name alias should be
        // ignored when resolving a release title.
        let aliases = vec![
            alias("Some Artist Alias", Some("en"), true, Some("Artist name")),
            alias("Summer Grass", Some("en"), true, Some("Release name")),
        ];
        let got = pick_preferred_name(
            "夏草が邪魔をする",
            None,
            &aliases,
            NameScriptPreference::Latin,
            AliasKind::Release,
        );
        assert_eq!(got, "Summer Grass");
    }

    #[test]
    fn latin_tolerates_untyped_aliases() {
        // MB sometimes returns aliases with no `type` field — still usable.
        let aliases = vec![MbAlias {
            name: "Yorushika".to_string(),
            locale: Some("en".to_string()),
            primary: true,
            ..Default::default()
        }];
        let got = pick_preferred_name(
            "ヨルシカ",
            None,
            &aliases,
            NameScriptPreference::Latin,
            AliasKind::Artist,
        );
        assert_eq!(got, "Yorushika");
    }
}
