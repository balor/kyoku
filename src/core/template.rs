use std::path::PathBuf;

/// All variables available for path template rendering.
#[derive(Debug, Clone, Default)]
pub struct TemplateVars {
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub year: String,
    pub title: String,
    pub track: u32,
    pub disc: u32,
    pub genre: String,
    pub ext: String,
    pub label: String,
    pub collection: String,
}

/// Render a path template by replacing `{variable}` and `{variable:format}`
/// placeholders with sanitized values from `vars`.
///
/// Supported format specifiers:
/// - `{track:02}` → zero-padded to 2 digits
/// - `{disc:0}`   → plain number (no padding)
/// - `{year:4}`   → 4-digit year
pub fn render_path(template: &str, vars: &TemplateVars) -> PathBuf {
    let mut result = template.to_string();

    // Process each {var} or {var:format} placeholder
    result = replace_var(&result, "album_artist", fallback(&vars.album_artist, &vars.artist));
    result = replace_var(&result, "artist", fallback(&vars.artist, "Unknown"));
    result = replace_var(&result, "album", fallback(&vars.album, "Unknown Album"));
    result = replace_var(&result, "year", fallback(&vars.year, "0000"));
    result = replace_var(&result, "title", fallback(&vars.title, "Unknown"));
    result = replace_var(&result, "genre", fallback(&vars.genre, "Unknown"));
    result = replace_var(&result, "label", fallback(&vars.label, "Unknown"));
    result = replace_var(&result, "collection", fallback(&vars.collection, ""));
    result = replace_var(&result, "ext", &vars.ext.to_lowercase());

    // Numeric variables with format specifiers
    result = replace_num_var(&result, "track", vars.track);
    result = replace_num_var(&result, "disc", vars.disc);

    PathBuf::from(result)
}

fn fallback<'a>(val: &'a str, default: &'a str) -> &'a str {
    if val.is_empty() { default } else { val }
}

/// Replace `{name}` with the sanitized value.
fn replace_var(template: &str, name: &str, value: &str) -> String {
    let sanitized = sanitize_path_component(value);
    let plain = format!("{{{}}}", name);
    template.replace(&plain, &sanitized)
}

/// Replace `{name}` and `{name:NN}` with a formatted number.
fn replace_num_var(template: &str, name: &str, value: u32) -> String {
    let mut result = template.to_string();

    // Look for {name:NN} patterns
    let prefix = format!("{{{}:", name);
    while let Some(start) = result.find(&prefix) {
        let rest = &result[start + prefix.len()..];
        if let Some(end) = rest.find('}') {
            let format_spec = &rest[..end];
            let formatted = format_number(value, format_spec);
            let full_pattern = format!("{}{}}}",  prefix, format_spec);
            result = result.replacen(&full_pattern, &formatted, 1);
        } else {
            break;
        }
    }

    // Plain {name}
    let plain = format!("{{{}}}", name);
    result = result.replace(&plain, &value.to_string());

    result
}

fn format_number(value: u32, spec: &str) -> String {
    // "02" → zero-pad to 2, "0" → plain, "4" → pad to 4
    if spec == "0" {
        return value.to_string();
    }
    if let Some(stripped) = spec.strip_prefix('0')
        && let Ok(width) = stripped.parse::<usize>() {
            return format!("{:0>width$}", value, width = width);
        }
    if let Ok(width) = spec.parse::<usize>() {
        return format!("{:>width$}", value, width = width);
    }
    value.to_string()
}

/// Sanitize a string for use as a filesystem path component.
///
/// Rules from spec §7:
/// 1. Replace / \ : * ? " < > | with _
/// 2. Trim leading/trailing whitespace and dots
/// 3. Collapse multiple consecutive underscores
/// 4. Truncate to 255 bytes (with hash suffix if needed)
/// 5. Handle Unicode correctly (no ASCII folding)
pub fn sanitize_path_component(s: &str) -> String {
    let mut result = String::with_capacity(s.len());

    for c in s.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => result.push('_'),
            _ => result.push(c),
        }
    }

    // Trim leading/trailing whitespace and dots
    let trimmed = result.trim_matches(|c: char| c.is_whitespace() || c == '.');

    // Collapse consecutive underscores
    let mut collapsed = String::with_capacity(trimmed.len());
    let mut last_was_underscore = false;
    for c in trimmed.chars() {
        if c == '_' {
            if !last_was_underscore {
                collapsed.push('_');
            }
            last_was_underscore = true;
        } else {
            collapsed.push(c);
            last_was_underscore = false;
        }
    }

    // Truncate to 255 bytes
    if collapsed.len() > 255 {
        // Find a safe truncation point (don't split UTF-8)
        let mut byte_count = 0;
        let mut char_end = 0;
        for (i, c) in collapsed.char_indices() {
            byte_count += c.len_utf8();
            if byte_count > 245 {
                char_end = i;
                break;
            }
        }
        if char_end > 0 {
            let hash = simple_hash(&collapsed);
            collapsed = format!("{}_{:08x}", &collapsed[..char_end], hash);
        }
    }

    if collapsed.is_empty() {
        return "Unknown".to_string();
    }

    collapsed
}

fn simple_hash(s: &str) -> u32 {
    let mut hash: u32 = 0;
    for b in s.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(b as u32);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_template() {
        let vars = TemplateVars {
            album_artist: "Radiohead".to_string(),
            album: "OK Computer".to_string(),
            year: "1997".to_string(),
            title: "Paranoid Android".to_string(),
            track: 2,
            disc: 1,
            ext: "flac".to_string(),
            ..Default::default()
        };
        let path = render_path("{album_artist}/{album} ({year})/{track:02} {title}.{ext}", &vars);
        assert_eq!(
            path,
            PathBuf::from("Radiohead/OK Computer (1997)/02 Paranoid Android.flac")
        );
    }

    #[test]
    fn missing_vars_use_fallbacks() {
        let vars = TemplateVars {
            title: "Song".to_string(),
            track: 1,
            ext: "mp3".to_string(),
            ..Default::default()
        };
        let path = render_path("{album_artist}/{album}/{track:02} {title}.{ext}", &vars);
        assert_eq!(
            path,
            PathBuf::from("Unknown/Unknown Album/01 Song.mp3")
        );
    }

    #[test]
    fn album_artist_falls_back_to_artist() {
        let vars = TemplateVars {
            artist: "Björk".to_string(),
            album: "Homogenic".to_string(),
            title: "Joga".to_string(),
            track: 1,
            ext: "flac".to_string(),
            ..Default::default()
        };
        let path = render_path("{album_artist}/{title}.{ext}", &vars);
        assert_eq!(path, PathBuf::from("Björk/Joga.flac"));
    }

    #[test]
    fn sanitizes_dangerous_chars() {
        let vars = TemplateVars {
            artist: "AC/DC".to_string(),
            title: "What's Next?".to_string(),
            ext: "mp3".to_string(),
            ..Default::default()
        };
        let path = render_path("{artist}/{title}.{ext}", &vars);
        assert_eq!(path, PathBuf::from("AC_DC/What's Next_.mp3"));
    }

    #[test]
    fn cjk_characters_preserved() {
        let vars = TemplateVars {
            album_artist: "初音ミク".to_string(),
            album: "VOCALOID BEST".to_string(),
            title: "千本桜".to_string(),
            track: 3,
            ext: "mp3".to_string(),
            ..Default::default()
        };
        let path = render_path("{album_artist}/{album}/{track:02} {title}.{ext}", &vars);
        assert_eq!(
            path,
            PathBuf::from("初音ミク/VOCALOID BEST/03 千本桜.mp3")
        );
    }

    #[test]
    fn format_specifiers() {
        let vars = TemplateVars {
            track: 7,
            disc: 2,
            ..Default::default()
        };
        assert_eq!(
            render_path("{disc:0}-{track:02}", &vars),
            PathBuf::from("2-07")
        );
    }
}
