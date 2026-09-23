//! JSON-RPC over a byte stream, as the protocol frames it.
//!
//! Each message is a `Content-Length` header, a blank line, and that many bytes
//! of JSON. Nothing here knows what any message means.

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// The largest message nun will read. A server announcing more than this has
/// lost its framing, and reading it would only exhaust memory on the way to
/// finding that out.
const LARGEST: usize = 256 * 1024 * 1024;

/// One message, sorted by what it is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Message {
    /// Something the other side wants an answer to.
    Request {
        /// Its id, echoed in the answer as it came: a number or a string.
        id: Value,
        /// What it is asking.
        method: String,
        /// With what.
        params: Value,
    },
    /// Something the other side is saying, wanting no answer.
    Notification {
        /// What it is about.
        method: String,
        /// The details.
        params: Value,
    },
    /// An answer to a request of ours.
    Response {
        /// The id of the request it answers.
        id: Value,
        /// The result, or the error in its place.
        result: Result<Value, Failure>,
    },
}

/// An error in place of a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    /// The error code.
    pub code: i64,
    /// What went wrong, in the server's words.
    pub message: String,
}

/// `RequestCancelled`: the code a request cancelled by `$/cancelRequest` is
/// answered with. nun drops such answers unread, so only the fake uses it.
#[cfg(test)]
pub(crate) const REQUEST_CANCELLED: i64 = -32800;

/// `MethodNotFound`.
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;

/// `InvalidParams`.
pub(crate) const INVALID_PARAMS: i64 = -32602;

impl Message {
    /// Sort a parsed JSON value. `None` for something that is none of the
    /// three: no method and no id, or an id with neither a result nor an error.
    pub(crate) fn from_value(mut value: Value) -> Option<Self> {
        let object = value.as_object_mut()?;
        let id = object.remove("id");
        let method = object.remove("method");
        let params = object.remove("params").unwrap_or(Value::Null);
        match (id, method) {
            (Some(id), Some(Value::String(method))) => Some(Self::Request { id, method, params }),
            (None, Some(Value::String(method))) => Some(Self::Notification { method, params }),
            (Some(id), None) => {
                let result = if let Some(error) = object.remove("error") {
                    Err(Failure {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                } else {
                    // A missing result is `null`, which is a perfectly good
                    // answer to most requests: "nothing here".
                    Ok(object.remove("result").unwrap_or(Value::Null))
                };
                Some(Self::Response { id, result })
            }
            _ => None,
        }
    }

    /// As JSON, ready to frame.
    ///
    /// Parameters of `null` are left out rather than sent: JSON-RPC allows
    /// them to be absent or structured, not null, and `shutdown` and `exit`
    /// have none.
    pub(crate) fn to_value(&self) -> Value {
        let mut value = match self {
            Self::Request { id, method, params } => {
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
            }
            Self::Notification { method, params } => {
                json!({ "jsonrpc": "2.0", "method": method, "params": params })
            }
            Self::Response { id, result: Ok(result) } => {
                json!({ "jsonrpc": "2.0", "id": id, "result": result })
            }
            Self::Response { id, result: Err(failure) } => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": failure.code, "message": failure.message },
            }),
        };
        if let Some(object) = value.as_object_mut()
            && object.get("params").is_some_and(Value::is_null)
        {
            object.remove("params");
        }
        value
    }
}

/// A message's JSON with its header in front, as it goes on the wire.
pub(crate) fn frame(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Why reading stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadError {
    /// The stream ended between messages: the other side went away.
    Closed,
    /// The stream broke, or said something that is not a message.
    Broken(String),
}

/// Read one message's body.
///
/// # Errors
///
/// [`ReadError::Closed`] at the end of the stream between messages, and
/// [`ReadError::Broken`] for anything else wrong: a stream that ends inside a
/// message, a header with no length, a length past [`LARGEST`], or a body that
/// is not JSON.
pub(crate) async fn read<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Value, ReadError> {
    let broken = |why: String| ReadError::Broken(why);
    let mut length = None;
    let mut line = String::new();
    let mut headers = 0usize;
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await.map_err(|error| broken(error.to_string()))?;
        if read == 0 {
            return Err(if headers == 0 {
                ReadError::Closed
            } else {
                broken("the stream ended inside a header".into())
            });
        }
        headers += 1;
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(broken(format!("not a header: {header:?}")));
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| broken(format!("not a length: {:?}", value.trim())))?,
            );
        }
    }
    let length = length.ok_or_else(|| broken("a message with no Content-Length".into()))?;
    if length > LARGEST {
        return Err(broken(format!("a message of {length} bytes")));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.map_err(|error| broken(error.to_string()))?;
    serde_json::from_slice(&body).map_err(|error| broken(format!("not JSON: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_all(bytes: &[u8]) -> Vec<Result<Value, ReadError>> {
        let mut reader = tokio::io::BufReader::new(bytes);
        let mut out = Vec::new();
        loop {
            let message = read(&mut reader).await;
            let done = message.is_err();
            out.push(message);
            if done {
                return out;
            }
        }
    }

    #[tokio::test]
    async fn framed_messages_come_back_one_at_a_time() {
        let mut bytes = frame(r#"{"a":1}"#);
        bytes.extend(frame(r#"{"b":"😀"}"#));
        let read = read_all(&bytes).await;
        assert_eq!(read[0], Ok(json!({"a": 1})));
        assert_eq!(read[1], Ok(json!({"b": "😀"})), "the length counts bytes, not chars");
        assert_eq!(read[2], Err(ReadError::Closed));
    }

    #[tokio::test]
    async fn other_headers_and_any_case_are_accepted() {
        let bytes =
            b"content-length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}";
        assert_eq!(read_all(bytes).await[0], Ok(json!({})));
    }

    #[tokio::test]
    async fn a_stream_cut_off_inside_a_message_is_broken_not_closed() {
        for bytes in [&b"Content-Length: 10\r\n\r\n{}"[..], b"Content-Length: 10\r\n"] {
            assert!(matches!(read_all(bytes).await[0], Err(ReadError::Broken(_))), "{bytes:?}");
        }
    }

    #[tokio::test]
    async fn nonsense_is_broken() {
        for bytes in [
            &b"Content-Length: x\r\n\r\n"[..],
            b"Content-Type: nothing\r\n\r\n",
            b"Content-Length: 3\r\n\r\n{{{",
            b"Content-Length: 999999999999\r\n\r\n",
            b"no colon\r\n\r\n",
        ] {
            assert!(matches!(read_all(bytes).await[0], Err(ReadError::Broken(_))), "{bytes:?}");
        }
    }

    #[test]
    fn messages_are_sorted_by_their_shape() {
        let request = json!({"jsonrpc": "2.0", "id": "a", "method": "m", "params": [1]});
        assert_eq!(
            Message::from_value(request),
            Some(Message::Request { id: json!("a"), method: "m".into(), params: json!([1]) })
        );
        let note = json!({"jsonrpc": "2.0", "method": "n"});
        assert_eq!(
            Message::from_value(note),
            Some(Message::Notification { method: "n".into(), params: Value::Null })
        );
        let answer = json!({"jsonrpc": "2.0", "id": 3, "result": null});
        assert_eq!(
            Message::from_value(answer),
            Some(Message::Response { id: json!(3), result: Ok(Value::Null) })
        );
        let error = json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32800, "message": "no"}});
        assert_eq!(
            Message::from_value(error),
            Some(Message::Response {
                id: json!(3),
                result: Err(Failure { code: REQUEST_CANCELLED, message: "no".into() })
            })
        );
        assert_eq!(Message::from_value(json!({"jsonrpc": "2.0"})), None);
        assert_eq!(Message::from_value(json!([1, 2])), None);
    }

    #[test]
    fn null_parameters_are_left_out() {
        let exit = Message::Notification { method: "exit".into(), params: Value::Null };
        assert_eq!(exit.to_value(), json!({ "jsonrpc": "2.0", "method": "exit" }));
        let shutdown =
            Message::Request { id: json!(1), method: "shutdown".into(), params: Value::Null };
        assert!(shutdown.to_value().get("params").is_none());
    }

    #[test]
    fn messages_survive_the_round_trip() {
        for message in [
            Message::Request { id: json!(1), method: "m".into(), params: json!({"x": 1}) },
            Message::Notification { method: "n".into(), params: json!(null) },
            Message::Response { id: json!("s"), result: Ok(json!([1])) },
            Message::Response {
                id: json!(2),
                result: Err(Failure { code: METHOD_NOT_FOUND, message: "what".into() }),
            },
        ] {
            assert_eq!(Message::from_value(message.to_value()), Some(message));
        }
    }
}
