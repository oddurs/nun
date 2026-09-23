//! File paths as `file:` URIs and back.
//!
//! Servers name documents by URI and nun names them by path. The two have to
//! agree byte for byte in one direction — a server matches the URI it is sent
//! against the URIs it knows — and survive a server's own choice of escaping
//! in the other, which is why incoming URIs are turned back into paths before
//! anything is looked up by them.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use lsp_types::Uri;

/// Bytes that go into a path segment unescaped: RFC 3986's unreserved set, and
/// the separator. Everything else is percent-encoded, which is always allowed
/// and never ambiguous.
fn plain(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// The `file:` URI for an absolute path.
///
/// `None` for a relative path, which has no URI: the caller makes it absolute
/// first, against the directory it is relative to.
#[must_use]
pub fn from_path(path: &Path) -> Option<Uri> {
    if !path.is_absolute() {
        return None;
    }
    let mut uri = String::from("file://");
    for &byte in &bytes_of(path) {
        if plain(byte) {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    Uri::from_str(&uri).ok()
}

/// The path a `file:` URI names, if it names one on this machine.
#[must_use]
pub fn to_path(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str();
    let rest = text.strip_prefix("file://")?;
    // `file://localhost/x` and `file:///x` are the same file.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(rest.len());
    let mut input = rest.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = input.next().and_then(hex)?;
            let low = input.next().and_then(hex)?;
            bytes.push(high << 4 | low);
        } else if byte == b'?' || byte == b'#' {
            // A query or a fragment is not part of the path.
            break;
        } else {
            bytes.push(byte);
        }
    }
    Some(PathBuf::from(os_string(bytes)?))
}

fn hex(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).and_then(|digit| u8::try_from(digit).ok())
}

#[cfg(unix)]
fn bytes_of(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn bytes_of(path: &Path) -> Vec<u8> {
    path.to_string_lossy().replace('\\', "/").into_bytes()
}

#[cfg(unix)]
#[expect(clippy::unnecessary_wraps, reason = "the same shape as the non-unix version")]
fn os_string(bytes: Vec<u8>) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Some(OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn os_string(bytes: Vec<u8>) -> Option<OsString> {
    String::from_utf8(bytes).ok().map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(path: &str) -> String {
        let uri = from_path(Path::new(path)).expect("absolute");
        assert_eq!(to_path(&uri).as_deref(), Some(Path::new(path)), "{}", uri.as_str());
        uri.as_str().to_string()
    }

    #[test]
    fn a_plain_path_is_spelled_as_it_is() {
        assert_eq!(round_trip("/home/me/src/main.rs"), "file:///home/me/src/main.rs");
    }

    #[test]
    fn spaces_unicode_and_reserved_characters_are_escaped_and_come_back() {
        assert_eq!(round_trip("/tmp/a b/c#d?.rs"), "file:///tmp/a%20b/c%23d%3F.rs");
        assert_eq!(round_trip("/tmp/ñandú/😀.rs"), "file:///tmp/%C3%B1and%C3%BA/%F0%9F%98%80.rs");
        round_trip("/tmp/100%/[x]:@!$&'()*+,;=.rs");
    }

    #[test]
    fn a_relative_path_has_no_uri() {
        assert!(from_path(Path::new("src/main.rs")).is_none());
    }

    #[test]
    fn a_server_escaping_differently_names_the_same_file() {
        // Some servers escape the colon of a drive letter or leave a space as
        // it is; the path is what gets compared, not the spelling.
        for spelling in
            ["file:///tmp/a%20b.rs", "file://localhost/tmp/a%20b.rs", "file:///tmp/a%20b.rs#L3"]
        {
            let uri = Uri::from_str(spelling).expect("valid");
            assert_eq!(to_path(&uri).as_deref(), Some(Path::new("/tmp/a b.rs")), "{spelling}");
        }
    }

    #[test]
    fn something_that_is_not_a_local_file_is_not_a_path() {
        for other in ["https://example.com/a.rs", "untitled:Untitled-1", "file://server/share/a.rs"]
        {
            let uri = Uri::from_str(other).expect("valid");
            assert_eq!(to_path(&uri), None, "{other}");
        }
    }
}
