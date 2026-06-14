# Test audio fixtures

The checked-in files under `sample_library/` are intentionally tiny fixtures used by the tag reader and import/organize tests. Tests fail if these files are missing.

Regenerate the MP3 fixtures with:

```sh
cargo run --bin create_fixtures
```

Regenerate the FLAC/OGG fixtures with ffmpeg, then tag them with Vorbis comments:

```sh
ffmpeg -y -f lavfi -i anullsrc=d=0.1 -c:a flac sample_library/tagged.flac
ffmpeg -y -f lavfi -i anullsrc=d=0.1 -c:a libvorbis sample_library/tagged.ogg
```

Expected tags for both `tagged.flac` and `tagged.ogg`:

- `TITLE=Test Title`
- `ARTIST=Test Artist`
- `ALBUM=Test Album`
- `DATE=2024`
- `TRACKNUMBER=1`

The committed FLAC/OGG files are minimal valid containers with those Vorbis comments so the repository does not require ffmpeg just to run tests.
