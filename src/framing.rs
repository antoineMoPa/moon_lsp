//! The envelope every language server message travels in: a `Content-Length` header, a
//! blank line, and that many bytes of JSON.
//!
//! Bytes arrive from a pipe in whatever sizes the kernel hands over - half a header, three
//! messages at once - so decoding is a buffer that is fed reads and asked for whole
//! messages, rather than anything that reads from the pipe itself. That is also what makes
//! it testable without a server.

const HEADER_END: &[u8] = b"\r\n\r\n";
const CONTENT_LENGTH: &str = "content-length:";

/// How long a header block may run before it is not a header. A real one is a
/// `Content-Length` line and perhaps a `Content-Type` beside it: well under a hundred bytes.
pub const MAX_HEADER_BYTES: usize = 8 * 1024;

/// The largest message a server may send. Far past anything a real one sends - a whole
/// workspace's symbols, a large file's semantic tokens - and far short of what would hurt to
/// hold, since the body is buffered whole before it is read.
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Why the stream from a server can be read no further. Each ends it: past a broken frame
/// there is no telling where the next one starts, so the reader closes the server rather than
/// guessing its way back in.
#[derive(Debug, PartialEq, Eq)]
pub enum FramingError {
    /// No blank line ending the header within [`MAX_HEADER_BYTES`].
    HeaderTooLong,
    /// A header block with no `Content-Length` that reads as a length.
    NoContentLength {
        /// The header block as it arrived.
        header: String,
    },
    /// A declared body past [`MAX_BODY_BYTES`].
    BodyTooLarge {
        /// The length the header gave.
        declared: usize,
    },
}

impl std::fmt::Display for FramingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HeaderTooLong => write!(
                formatter,
                "the server sent {MAX_HEADER_BYTES} bytes without ending a message header"
            ),
            Self::NoContentLength { header } => {
                write!(formatter, "the server sent a header with no Content-Length: {header:?}")
            }
            Self::BodyTooLarge { declared } => write!(
                formatter,
                "the server declared a {declared}-byte message, past the {MAX_BODY_BYTES}-byte limit"
            ),
        }
    }
}

impl std::error::Error for FramingError {}

/// One message, wrapped for the wire.
pub fn frame(body: &str) -> Vec<u8> {
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(body.as_bytes());
    framed
}

/// What has arrived from the server and not yet been read out as messages.
#[derive(Default)]
pub struct Frames {
    buffer: Vec<u8>,
}

impl Frames {
    /// Take what one read off the pipe gave.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole message, if a whole one has arrived. `Ok(None)` means the rest of it is
    /// still on its way. An error means the stream is broken for good - see [`FramingError`] -
    /// and nothing more is read from it.
    ///
    /// Every limit is checked before the buffer is allowed to grow past it: a header is given
    /// up on at [`MAX_HEADER_BYTES`], and a body is refused on its declared length, before any
    /// of it is waited for.
    pub fn next_message(&mut self) -> Result<Option<String>, FramingError> {
        let Some(header_end) = find(&self.buffer, HEADER_END) else {
            if self.buffer.len() > MAX_HEADER_BYTES {
                return Err(FramingError::HeaderTooLong);
            }
            return Ok(None);
        };
        if header_end > MAX_HEADER_BYTES {
            return Err(FramingError::HeaderTooLong);
        }
        let header = String::from_utf8_lossy(&self.buffer[..header_end]).to_string();
        let Some(length) = content_length(&header) else {
            return Err(FramingError::NoContentLength { header });
        };
        if length > MAX_BODY_BYTES {
            return Err(FramingError::BodyTooLarge { declared: length });
        }

        // Both terms are bounded above, so neither sum can overflow.
        let body_start = header_end + HEADER_END.len();
        let message_end = body_start + length;
        if self.buffer.len() < message_end {
            return Ok(None);
        }
        let body: Vec<u8> = self.buffer.drain(..message_end).collect();
        Ok(Some(String::from_utf8_lossy(&body[body_start..]).to_string()))
    }
}

/// The length off a header block. Header names are case-insensitive, and a server may send
/// a `Content-Type` line beside the length.
fn content_length(header: &str) -> Option<usize> {
    header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            format!("{}:", name.trim().to_ascii_lowercase())
                .eq(CONTENT_LENGTH)
                .then_some(value)
        })?
        .trim()
        .parse()
        .ok()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
