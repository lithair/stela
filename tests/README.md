# Lifecycle checks

`asset_lifecycle.py` is a probatum helper using Python 3's standard library.
It provides PUT/DELETE and service restarts, which probatum 0.9 cannot express
natively. Python is a test dependency only. Each invocation creates its own
temporary blog, chooses a loopback port, stops its servers, and removes its
data on completion. Failed checks print the reason and the last server logs;
probatum retains that output as evidence.

From the repository root, after building the musl release binary:

```bash
cidx run probatum-runner
cidx run probatum
```

The main `probatum.toml` includes the passing unpublish scenario for posts and
pages: publish, return to draft, wait for the mutation hook, rebuild explicitly,
restart, republish at the same URL, and restart again. Public reads are made
without the admin cookie. It also checks withdrawn posts leave the index/RSS
and that unrelated content and theme assets survive.

The CI test phase builds the musl artifact and copies the pinned probatum
binary from its official image to `target/probatum-runner/`, then runs the
manifest in a Python Alpine container. It needs neither pip nor a privileged
package installation. The application image remains unchanged.

For a focused local run against another build:

```bash
cargo build --locked
python3 tests/asset_lifecycle.py --binary target/debug/stela
```

## Known failing DELETE regression

```bash
probatum run tests/probatum-delete.toml
```

This uses the musl release binary and requires local Python 3. It currently
**fails** on Lithair 1.10.0: model replay reinserts Deleted event payloads, so
the public URL stays readable. It is intentionally separate from the passing
CI suite, with no expected-failure exit-code inversion. Once fixed upstream,
move its check into the main manifest. The scenario checks actual removal,
restart persistence, and recreation, just as for unpublishing.

For a different binary, run the helper with both `--binary` and
`--scenario delete`. Do not weaken its expected 404 to match the known bug.
