# Trust roots

`cacert.pem` is Mozilla's CA certificate store as extracted by curl, the same
source ESP-IDF's certificate bundle uses. Every Platform trusts exactly these
roots; no Platform reads its operating system's certificate store.

| Field | Value |
| --- | --- |
| Source | <https://curl.se/ca/cacert.pem> |
| Mozilla data as of | Fri Sep 25 03:12:01 2026 GMT |
| Certificates | 121 |
| SHA-256 | `a41b5d356aea97a529fe27e0f7316d2f9d946d75927476cf9cf1b90637d00505` |

To update, replace `cacert.pem` with the current file from the source above,
check it against the published `cacert.pem.sha256`, and update this table.
