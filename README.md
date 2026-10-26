# fdroid-watch

A terminal dashboard showing where each Pocket Sense release is on its way to F-Droid:
tagged on GitHub → picked up in fdroiddata by the update bot → built in a build cycle → live on f-droid.org.

## Install

Needs Rust ([rustup](https://rustup.rs)).

```sh
cargo install --git https://github.com/software-ace/fdroid-watch
fdroid-watch
```

No clone needed: cargo fetches the source, builds it and puts `fdroid-watch` in `~/.cargo/bin`, or in `$CARGO_INSTALL_ROOT/bin` if that's set. That directory has to be on your `PATH` (fish: `fish_add_path ~/.cargo/bin`). Run the same command again to update; `cargo uninstall fdroid-watch` removes it.

From a clone, `cargo install --path .` does the same with your local changes.

To try it without installing: `cargo run`.

## Use

It fetches on start and every 15 minutes in the background; `r` refreshes now, `q` quits.

Each version has a cat walking its tag → bot → build → live track, with GitHub's release date and the date f-droid.org added it, or an estimate (`~`): the GitHub date plus the median delay of past releases. The big cat shows the mood: hunting while fetching, napping while F-Droid takes its days, purring once the newest release is live, hissing if a build failed.

Sources, all public, no tokens:

- GitHub tags and releases of `software-ace/PocketSense`
- `metadata/ace.software.pocketsense.yml` in fdroiddata
- `f-droid.org/api/v1/packages/ace.software.pocketsense`
- the package page on f-droid.org, for the "Added on" dates the API leaves out
- `f-droid.org/repo/status/build.json` (last build cycle) and `running.json` (current one)

`cargo test` checks the parsing; `cargo test -- --ignored --nocapture` draws one live fetch as text.
