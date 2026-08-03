# packaging/homebrew — the `balor/kyoku` tap

Formula for installing kyoku via Homebrew from the prebuilt GitHub release
tarballs (no Rust toolchain needed for users). **Homebrew Option A**: our own
tap, not homebrew-core — no notability requirements, we control updates.

## One-time: create the tap repo

Homebrew maps GitHub repo `<user>/homebrew-<name>` to tap `<user>/<name>`.
So the tap `balor/kyoku` lives at `github.com/balor/homebrew-kyoku`,
with this directory's contents at its root (`Formula/kyoku.rb`):

```sh
gh repo create balor/homebrew-kyoku --public \
    --description "Homebrew tap for kyoku (TUI music library manager)"
git clone git@github.com:balor/homebrew-kyoku.git
cp -r packaging/homebrew/Formula homebrew-kyoku/
cd homebrew-kyoku && git add Formula && git commit -m "kyoku 0.2.1" && git push
```

Users then install with:

```sh
brew install balor/kyoku/kyoku
# or: brew tap balor/kyoku && brew install kyoku
```

## Per-release: update the formula

After pushing tag `vX.Y.Z` to GitHub and the release workflow has finished
(the three tap-relevant assets must exist: both macOS tarballs and the
x86_64 Linux tarball):

```sh
./update-formula.sh X.Y.Z      # downloads assets, hashes them, rewrites Formula/kyoku.rb
# then copy/commit/push Formula/kyoku.rb to the balor/homebrew-kyoku repo
```

Until the sha256 placeholders in `Formula/kyoku.rb` are replaced with real
hashes, installs fail with a checksum mismatch — that is intentional
(`0000…` is a tripwire, not a value Homebrew could ever accept silently).

## Layout

- `Formula/kyoku.rb` — binary formula; per-arch URLs + sha256, `bin.install "kyoku"`
- `update-formula.sh` — post-release version/hash bumper (see above)