# fdroid-watch

A terminal dashboard showing where each Pocket Sense release is on its way to F-Droid:
tagged on GitHub → picked up in fdroiddata by the update bot → built in a build cycle → live on f-droid.org.

```sh
cargo run --release
```

`r` refreshes, `q` quits. It also refreshes by itself every 15 minutes.

Sources, all public, no tokens:

- GitHub tags and releases of `software-ace/PocketSense`
- `metadata/ace.software.pocketsense.yml` in fdroiddata
- `f-droid.org/api/v1/packages/ace.software.pocketsense`
- `f-droid.org/repo/status/build.json` (last build cycle) and `running.json` (current one)

`cargo test` checks the parsing; `cargo test -- --ignored --nocapture` draws one live fetch as text.
