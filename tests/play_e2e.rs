//! End-to-end play test: a fake player (shell script) stands in for mpv,
//! recording the argv it was launched with and the playlist it was given.
//! Verifies the whole pipeline — resolution via `[player].command`,
//! playlist writing, template substitution, spawn, outcome counting —
//! without touching a real player.

use kyoku::core::player::{self, PlayItem};
use kyoku::config::Settings;
use std::path::PathBuf;

/// `play()` spawns fire-and-forget, so the fake player's output files
/// appear a few ms later. Poll briefly instead of racing the child.
fn wait_for_file(path: &std::path::Path) -> String {
    for _ in 0..100 {
        if let Ok(content) = std::fs::read_to_string(path)
            && !content.is_empty()
        {
            return content;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}

fn fake_player_script(tmp: &tempfile::TempDir) -> PathBuf {
    let script = tmp.path().join("fake-mpv.sh");
    let argv_out = tmp.path().join("argv.txt");
    let playlist_out = tmp.path().join("playlist.txt");
    let content = format!(
        "#!/bin/sh\n\
         printf '%s\\n' \"$@\" > \"{argv}\"\n\
         for a in \"$@\"; do\n\
           f=\"${{a#--playlist=}}\"\n\
           case \"$f\" in\n\
             *.m3u8) cat \"$f\" > \"{plist}\" ;;\n\
           esac\n\
         done\n",
        argv = argv_out.display(),
        plist = playlist_out.display(),
    );
    std::fs::write(&script, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

#[test]
fn play_launches_configured_command_with_playlist() {
    let tmp = tempfile::tempdir().unwrap();
    let script = fake_player_script(&tmp);

    // Two existing audio files.
    let a = tmp.path().join("01 靴の花火.flac");
    let b = tmp.path().join("02 ただ君に晴れ.flac");
    std::fs::write(&a, b"x").unwrap();
    std::fs::write(&b, b"x").unwrap();

    let mut settings = Settings::default();
    settings.player.command = Some(vec![
        script.display().to_string(),
        "--playlist={playlist}".to_string(),
    ]);

    let items = vec![
        PlayItem {
            path: a.clone(),
            title: "靴の花火".to_string(),
            artist: Some("ヨルシカ".to_string()),
            duration_ms: Some(183_000),
        },
        PlayItem {
            path: b.clone(),
            title: "ただ君に晴れ".to_string(),
            artist: Some("ヨルシカ".to_string()),
            duration_ms: Some(240_000),
        },
    ];

    let outcome = player::play(&settings, items).expect("play should succeed");
    assert_eq!(outcome.played, 2);
    assert_eq!(outcome.skipped_missing, 0);
    let playlist = outcome.playlist_path.expect("multi-item play writes a playlist");

    // The fake player saw: script-arg substituted with the playlist path.
    let argv = wait_for_file(&tmp.path().join("argv.txt"));
    assert_eq!(
        argv.trim(),
        format!("--playlist={}", playlist.display()),
        "fake player argv"
    );

    // And the playlist it was handed contains both CJK paths.
    let handed = wait_for_file(&tmp.path().join("playlist.txt"));
    assert!(handed.starts_with("#EXTM3U"));
    assert!(handed.contains("01 靴の花火.flac"));
    assert!(handed.contains("02 ただ君に晴れ.flac"));
    assert!(handed.contains("#EXTINF:183,ヨルシカ - 靴の花火"));
}

#[test]
fn play_single_track_opens_file_directly() {
    let tmp = tempfile::tempdir().unwrap();
    let script = fake_player_script(&tmp);
    let a = tmp.path().join("one.flac");
    std::fs::write(&a, b"x").unwrap();

    let mut settings = Settings::default();
    settings.player.command = Some(vec![script.display().to_string()]);

    let outcome = player::play(&settings, vec![PlayItem::from_path(a.clone())]).unwrap();
    assert_eq!(outcome.played, 1);
    assert!(outcome.playlist_path.is_none(), "single track → no playlist file");

    let argv = wait_for_file(&tmp.path().join("argv.txt"));
    assert_eq!(argv.trim(), a.display().to_string());
}

#[test]
fn play_all_missing_files_is_an_error_and_spawns_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let script = fake_player_script(&tmp);

    let mut settings = Settings::default();
    settings.player.command = Some(vec![script.display().to_string()]);

    let items = vec![PlayItem::from_path(tmp.path().join("nope.flac"))];
    let err = player::play(&settings, items).unwrap_err().to_string();
    assert!(err.contains("nothing playable"), "{err}");
    assert!(
        !tmp.path().join("argv.txt").exists(),
        "nothing should have been spawned"
    );
}
