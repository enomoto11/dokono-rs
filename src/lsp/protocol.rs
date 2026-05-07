//! LSP base protocol framing: `Content-Length: N\r\n\r\n<json>`.
//! `N` is the UTF-8 byte length of the JSON body, not its character count.

use anyhow::{Context, Result, bail};
use std::io::{BufRead, Write};

pub fn write_message<W: Write>(w: &mut W, body: &serde_json::Value) -> Result<()> {
    let payload = serde_json::to_vec(body).context("failed to serialize LSP message")?;
    write!(w, "Content-Length: {}\r\n\r\n", payload.len())?;
    w.write_all(&payload)?;
    w.flush()?;
    Ok(())
}

/// Returns `Ok(None)` for a clean EOF before any header bytes are read.
pub fn read_message<R: BufRead>(r: &mut R) -> Result<Option<serde_json::Value>> {
    let mut content_length: Option<usize> = None;
    let mut header_line = String::new();
    let mut headers_seen = 0usize;

    loop {
        header_line.clear();
        let n = r.read_line(&mut header_line)?;
        if n == 0 {
            if headers_seen == 0 {
                return Ok(None);
            }
            bail!("unexpected EOF in LSP headers");
        }
        if header_line == "\r\n" || header_line == "\n" {
            break;
        }
        headers_seen += 1;
        let trimmed = header_line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            let parsed: usize = rest
                .trim()
                .parse()
                .with_context(|| format!("malformed Content-Length: {trimmed:?}"))?;
            content_length = Some(parsed);
        }
    }

    let len = content_length.context("LSP message missing Content-Length header")?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)
        .context("LSP body short read (expected Content-Length bytes)")?;
    let value: serde_json::Value =
        serde_json::from_slice(&buf).context("LSP body was not valid JSON")?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn round_trip_ascii() {
        let msg = json!({"jsonrpc": "2.0", "method": "test", "params": {"a": 1}});
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let mut cur = Cursor::new(buf);
        let read = read_message(&mut cur).unwrap().unwrap();
        assert_eq!(read, msg);
    }

    #[test]
    fn content_length_uses_utf8_byte_length() {
        // Multibyte UTF-8 strings have different char and byte lengths; verify explicitly.
        let text = "こんにちは"; // 5 chars, 15 bytes (UTF-8)
        let msg = json!({"text": text});
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        let body_bytes = serde_json::to_vec(&msg).unwrap();
        let expected_header = format!("Content-Length: {}", body_bytes.len());
        let written = String::from_utf8_lossy(&buf);
        assert!(
            written.contains(&expected_header),
            "expected header `{expected_header}` in:\n{written}"
        );

        // round-trip
        let mut cur = Cursor::new(buf);
        let read = read_message(&mut cur).unwrap().unwrap();
        assert_eq!(read, msg);
        assert_eq!(read["text"], text);
    }

    #[test]
    fn eof_returns_none() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        assert!(read_message(&mut cur).unwrap().is_none());
    }

    #[test]
    fn multiple_messages_in_stream() {
        let m1 = json!({"id": 1});
        let m2 = json!({"id": 2});
        let mut buf = Vec::new();
        write_message(&mut buf, &m1).unwrap();
        write_message(&mut buf, &m2).unwrap();
        let mut cur = Cursor::new(buf);
        assert_eq!(read_message(&mut cur).unwrap().unwrap(), m1);
        assert_eq!(read_message(&mut cur).unwrap().unwrap(), m2);
        assert!(read_message(&mut cur).unwrap().is_none());
    }

    #[test]
    fn ignores_unknown_headers() {
        let body = br#"{"k":1}"#;
        let mut data = Vec::new();
        data.extend_from_slice(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n");
        data.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        data.extend_from_slice(body);
        let mut cur = Cursor::new(data);
        let read = read_message(&mut cur).unwrap().unwrap();
        assert_eq!(read, json!({"k": 1}));
    }
}
