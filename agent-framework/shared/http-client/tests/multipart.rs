#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use futures_lite::{future::block_on, stream, StreamExt};
use http_client::{Body, Multipart};

#[test]
fn encodes_text_and_in_memory_file_parts() {
    block_on(async {
        let multipart = Multipart::new("boundary-123").text("chat_id", "42").file(
            "document",
            "notes.txt",
            "text/plain",
            Body::Bytes(b"hello".to_vec()),
        );
        let result = multipart.finish();
        assert!(result.is_ok());
        let Ok((content_type, body)) = result else {
            return;
        };

        assert_eq!(content_type, "multipart/form-data; boundary=boundary-123");
        assert!(matches!(body, Body::Bytes(bytes) if bytes == concat!(
            "--boundary-123\r\n",
            "Content-Disposition: form-data; name=\"chat_id\"\r\n\r\n",
            "42\r\n",
            "--boundary-123\r\n",
            "Content-Disposition: form-data; name=\"document\"; filename=\"notes.txt\"\r\n",
            "Content-Type: text/plain\r\n\r\n",
            "hello\r\n",
            "--boundary-123--\r\n",
        ).as_bytes()));
    });
}

#[test]
fn preserves_streaming_file_content_without_buffering_it() {
    block_on(async {
        let file = stream::iter([Ok(b"abc".to_vec()), Ok(b"def".to_vec())]);
        let multipart = Multipart::new("b").text("caption", "hi").file(
            "photo",
            "p.jpg",
            "image/jpeg",
            Body::stream(Box::pin(file)),
        );
        let result = multipart.finish();
        assert!(result.is_ok());
        let Ok((_, body)) = result else {
            return;
        };
        let mut stream = match body {
            Body::Stream { stream, .. } => stream,
            _ => {
                panic!("expected streaming multipart body");
            }
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => bytes.extend_from_slice(&chunk),
                Err(_) => panic!("multipart stream failed"),
            }
        }

        let encoded = String::from_utf8_lossy(&bytes);
        assert!(encoded.contains("name=\"caption\"\r\n\r\nhi\r\n"));
        assert!(encoded.contains("name=\"photo\"; filename=\"p.jpg\""));
        assert!(encoded.contains("\r\n\r\nabcdef\r\n--b--\r\n"));
    });
}

#[test]
fn rejects_header_injection_in_part_metadata() {
    let multipart = Multipart::new("safe").text("bad\r\nInjected", "value");

    assert!(multipart.finish().is_err());
}
