# fdroid-watch

A terminal dashboard showing where each Pocket Sense release is on its way to F-Droid:
tagged on GitHub → picked up in fdroiddata by the update bot → built in a build cycle → live on f-droid.org.

```sh
cargo run --release
```

It fetches on start and every 15 minutes in the background; `r` refreshes now, `q` quits.

Each version has a cat walking its tag → bot → build → live track, with GitHub's release date and the date f-droid.org added it. The big cat shows the mood: hunting while fetching, napping while F-Droid takes its days, purring once the newest release is live, hissing if a build failed.

Sources, all public, no tokens:

- GitHub tags and releases of `software-ace/PocketSense`
- `metadata/ace.software.pocketsense.yml` in fdroiddata
- `f-droid.org/api/v1/packages/ace.software.pocketsense`
- the package page on f-droid.org, for the "Added on" dates the API leaves out
- `f-droid.org/repo/status/build.json` (last build cycle) and `running.json` (current one)

`cargo test` checks the parsing; `cargo test -- --ignored --nocapture` draws one live fetch as text.
