# mbedtls-rs, patched for Barracuda

This is [mbedtls-rs](https://github.com/esp-rs/mbedtls-rs) 0.1.0 as published
on crates.io (upstream commit `3b90d296aa4e28a1df22c9aaacc74b3d6bef859f`,
`mbedtls-rs/`), used through `[patch.crates-io]` in the workspace manifest.
The upstream tests and their `[[test]]` and `[dev-dependencies]` entries are
dropped; everything else is the published crate plus the changes below. They
are candidates to send upstream.

| Change | Files | Why |
| --- | --- | --- |
| `Certificate::verified_by(VerifyCallback)`: an empty CA chain plus a callback that `Session` installs with `mbedtls_ssl_conf_verify` | `src/cert.rs`, `src/session.rs` | `shared/tls` verifies against a compact root bundle by issuer lookup, as ESP-IDF's certificate bundle does, instead of parsing every root into RAM |
| `TlsRng` and `RngFailure`: `Tls::new` takes a random source that may fail; every `CryptoRng` still is one | `src/lib.rs` | A Platform entropy source can fail; MbedTLS then sees an entropy error rather than weak bytes or a panic |
