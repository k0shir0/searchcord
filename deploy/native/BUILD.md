# Build and publish Searchcord

Install Rust and the system C compiler. Run these commands from the repository
root. Cargo uses the pinned Rust toolchain and lockfile.

```sh
cargo build --release --locked -p searchcord-collector -p searchcord-viewer -p searchcord-snapshot
pwsh -File deploy/native/package.ps1
```

The package appears in `dist`. On Windows, `start.bat` and `start-search.bat`
also find a build under `target/release`. See the [operating guide](README.txt)
for direct launch, archive location and migration instructions.

## Publish a release

1. Set the version in the workspace `Cargo.toml` and refresh `Cargo.lock`.
2. After the change reaches `main`, run **Rust migration parity** from GitHub
   Actions on `main` and enter short user-facing release notes.
3. The workflow publishes `v<version>` with Windows x64, Linux and macOS
   downloads after all three jobs pass. Leaving the notes blank runs checks
   and builds packages for review.

**Update Searchcord** uses the latest stable GitHub release, verifies the Windows
ZIP against GitHub's SHA-256 digest, and activates the new program directory only
after all files are present. It keeps the previous program files and leaves the
archive and credential locations alone.
