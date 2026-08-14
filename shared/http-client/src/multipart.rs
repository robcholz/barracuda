use alloc::{boxed::Box, collections::VecDeque, format, string::String, vec::Vec};
use core::{pin::Pin, task::Poll};

use futures_core::Stream;

use crate::{Body, BodyError, BodyStream};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MultipartError {
    #[error("invalid multipart metadata")]
    InvalidMetadata,
    #[error("multipart body is too large")]
    LengthOverflow,
}

enum Part {
    Text {
        name: String,
        value: String,
    },
    File {
        name: String,
        filename: String,
        content_type: String,
        body: Body,
    },
}

pub struct Multipart {
    boundary: String,
    parts: Vec<Part>,
}

impl Multipart {
    pub fn new(boundary: impl Into<String>) -> Self {
        Self {
            boundary: boundary.into(),
            parts: Vec::new(),
        }
    }

    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.parts.push(Part::Text {
            name: name.into(),
            value: value.into(),
        });
        self
    }

    pub fn file(
        mut self,
        name: impl Into<String>,
        filename: impl Into<String>,
        content_type: impl Into<String>,
        body: Body,
    ) -> Self {
        self.parts.push(Part::File {
            name: name.into(),
            filename: filename.into(),
            content_type: content_type.into(),
            body,
        });
        self
    }

    pub fn finish(self) -> Result<(String, Body), MultipartError> {
        validate_boundary(&self.boundary)?;
        let content_type = format!("multipart/form-data; boundary={}", self.boundary);
        let mut segments = VecDeque::new();
        let mut contains_stream = false;

        for part in self.parts {
            match part {
                Part::Text { name, value } => {
                    validate_quoted(&name)?;
                    segments.push_back(Segment::Bytes(
                        format!(
                            "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
                            self.boundary, name, value
                        )
                        .into_bytes(),
                    ));
                }
                Part::File {
                    name,
                    filename,
                    content_type,
                    body,
                } => {
                    validate_quoted(&name)?;
                    validate_quoted(&filename)?;
                    validate_header_value(&content_type)?;
                    segments.push_back(Segment::Bytes(
                        format!(
                            "--{}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                            self.boundary, name, filename, content_type
                        )
                        .into_bytes(),
                    ));
                    match body {
                        Body::Empty => {}
                        Body::Bytes(bytes) => segments.push_back(Segment::Bytes(bytes)),
                        Body::Stream {
                            stream,
                            content_length,
                        } => {
                            contains_stream = true;
                            segments.push_back(Segment::Stream {
                                stream,
                                content_length,
                            });
                        }
                    }
                    segments.push_back(Segment::Bytes(b"\r\n".to_vec()));
                }
            }
        }
        segments.push_back(Segment::Bytes(
            format!("--{}--\r\n", self.boundary).into_bytes(),
        ));

        if !contains_stream {
            let mut bytes = Vec::new();
            for segment in segments {
                if let Segment::Bytes(chunk) = segment {
                    bytes.extend_from_slice(&chunk);
                }
            }
            return Ok((content_type, Body::Bytes(bytes)));
        }

        let content_length = content_length(&segments)?;
        let stream: BodyStream = Box::pin(MultipartStream { segments });
        let body = match content_length {
            Some(length) => Body::stream_with_length(stream, length),
            None => Body::stream(stream),
        };
        Ok((content_type, body))
    }
}

enum Segment {
    Bytes(Vec<u8>),
    Stream {
        stream: BodyStream,
        content_length: Option<usize>,
    },
}

fn content_length(segments: &VecDeque<Segment>) -> Result<Option<usize>, MultipartError> {
    let mut total = 0usize;
    for segment in segments {
        let length = match segment {
            Segment::Bytes(bytes) => bytes.len(),
            Segment::Stream {
                content_length: Some(length),
                ..
            } => *length,
            Segment::Stream {
                content_length: None,
                ..
            } => return Ok(None),
        };
        total = total
            .checked_add(length)
            .ok_or(MultipartError::LengthOverflow)?;
    }
    Ok(Some(total))
}

struct MultipartStream {
    segments: VecDeque<Segment>,
}

impl Stream for MultipartStream {
    type Item = Result<Vec<u8>, BodyError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        loop {
            match self.segments.front_mut() {
                Some(Segment::Bytes(_)) => {
                    let Some(Segment::Bytes(bytes)) = self.segments.pop_front() else {
                        return Poll::Ready(None);
                    };
                    return Poll::Ready(Some(Ok(bytes)));
                }
                Some(Segment::Stream { stream, .. }) => match stream.as_mut().poll_next(context) {
                    Poll::Ready(None) => {
                        self.segments.pop_front();
                    }
                    Poll::Ready(Some(item)) => return Poll::Ready(Some(item)),
                    Poll::Pending => return Poll::Pending,
                },
                None => return Poll::Ready(None),
            }
        }
    }
}

fn validate_boundary(boundary: &str) -> Result<(), MultipartError> {
    if boundary.is_empty()
        || !boundary
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(MultipartError::InvalidMetadata);
    }
    Ok(())
}

fn validate_quoted(value: &str) -> Result<(), MultipartError> {
    if value.is_empty() || value.contains(['\r', '\n', '"']) {
        return Err(MultipartError::InvalidMetadata);
    }
    Ok(())
}

fn validate_header_value(value: &str) -> Result<(), MultipartError> {
    if value.is_empty() || value.contains(['\r', '\n']) {
        return Err(MultipartError::InvalidMetadata);
    }
    Ok(())
}
