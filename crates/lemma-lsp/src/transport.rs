//! LSP base protocol framing: a `Content-Length` header, a blank line, then a
//! JSON body.

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// Reads one message. Returns `None` when the stream ends cleanly between
/// messages.
pub(crate) fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let Some(content_length) = read_content_length(reader)? else {
        return Ok(None);
    };
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    let message = serde_json::from_slice(&body).map_err(|error| invalid_data(error.to_string()))?;
    Ok(Some(message))
}

/// Writes one message with its header.
pub(crate) fn write_message(writer: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(|error| invalid_data(error.to_string()))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

/// Reads header lines up to the blank separator and returns the body length.
fn read_content_length(reader: &mut impl BufRead) -> io::Result<Option<usize>> {
    let mut content_length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return match content_length {
                None => Ok(None),
                Some(_) => Err(invalid_data(
                    "stream ended inside a message header".to_owned(),
                )),
            };
        }

        let header = line.trim_end();
        if header.is_empty() {
            return content_length
                .map(Some)
                .ok_or_else(|| invalid_data("message header has no Content-Length".to_owned()));
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            let length = value
                .trim()
                .parse()
                .map_err(|_| invalid_data(format!("invalid Content-Length `{value}`")))?;
            content_length = Some(length);
        }
    }
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn written_messages_read_back() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &json!({"id": 1, "result": "é"})).unwrap();
        write_message(&mut buffer, &json!({"method": "exit"})).unwrap();

        let mut reader = Cursor::new(buffer);
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Some(json!({"id": 1, "result": "é"}))
        );
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Some(json!({"method": "exit"}))
        );
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn content_length_counts_bytes_not_characters() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &json!("é")).unwrap();

        let header = String::from_utf8_lossy(&buffer);
        assert!(header.starts_with("Content-Length: 4\r\n\r\n"));
    }

    #[test]
    fn extra_headers_are_ignored() {
        let raw = "Content-Type: application/vscode-jsonrpc\r\nContent-Length: 2\r\n\r\n{}";
        let message = read_message(&mut Cursor::new(raw)).unwrap();
        assert_eq!(message, Some(json!({})));
    }

    #[test]
    fn missing_content_length_is_an_error() {
        let raw = "Content-Type: x\r\n\r\n{}";
        assert!(read_message(&mut Cursor::new(raw)).is_err());
    }

    #[test]
    fn truncated_header_is_an_error() {
        let raw = "Content-Length: 2\r\n";
        assert!(read_message(&mut Cursor::new(raw)).is_err());
    }
}
