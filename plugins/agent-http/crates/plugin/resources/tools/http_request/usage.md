Use `http_request` for direct HTTP or HTTPS access. The call waits for the
complete response and requires a UTF-8 body. An `error` result describes
validation, size, capacity, timeout, transport, text-decoding, or response-size
failure. URLs are limited to 2 KiB, request bodies to 32 KiB, and responses to
64 KiB; at most 32 headers may use 16 KiB in total.
