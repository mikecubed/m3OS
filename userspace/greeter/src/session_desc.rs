//! Phase 71 Track D.3 — session descriptor serialization.
//!
//! Wire format (greeter stdout → session_manager parser):
//!
//! ```text
//! uid=<N> gid=<N> home=<path> shell=<path>\n
//! ```
//!
//! Space-separated `key=value` pairs, one line, trailing newline.
//! Paths must not contain spaces (greeter passes them through `passwd`
//! verbatim; the `/etc/passwd` parser already rejects paths with spaces
//! because `:` is the field delimiter and shell strings are
//! whitespace-free in practice).

use alloc::string::String;

use crate::auth::SessionDescriptor;

/// Greeter side: format the descriptor into a single line.
pub fn format_session_descriptor(desc: &SessionDescriptor) -> String {
    // No format! because alloc has `format!` but kernel-core enforces
    // no_std + alloc, and `format!` ICEs are rare enough. Use it.
    alloc::format!(
        "uid={} gid={} home={} shell={}\n",
        desc.uid,
        desc.gid,
        desc.home,
        desc.shell
    )
}

/// Errors emitted by [`parse_session_descriptor`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Required key is missing or malformed.
    MissingField(&'static str),
    /// `uid` / `gid` value is not a valid decimal u32.
    BadInteger(&'static str),
}

/// session_manager side: parse the descriptor line. Tolerant of a
/// trailing `\n` and of leading whitespace.
pub fn parse_session_descriptor(line: &str) -> Result<SessionDescriptor, ParseError> {
    let mut uid: Option<u32> = None;
    let mut gid: Option<u32> = None;
    let mut home: Option<String> = None;
    let mut shell: Option<String> = None;
    for token in line.split_whitespace() {
        let Some(eq) = token.find('=') else {
            continue;
        };
        let key = &token[..eq];
        let value = &token[eq + 1..];
        match key {
            "uid" => {
                uid = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| ParseError::BadInteger("uid"))?,
                )
            }
            "gid" => {
                gid = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| ParseError::BadInteger("gid"))?,
                )
            }
            "home" => home = Some(String::from(value)),
            "shell" => shell = Some(String::from(value)),
            _ => {}
        }
    }
    Ok(SessionDescriptor {
        uid: uid.ok_or(ParseError::MissingField("uid"))?,
        gid: gid.ok_or(ParseError::MissingField("gid"))?,
        home: home.ok_or(ParseError::MissingField("home"))?,
        shell: shell.ok_or(ParseError::MissingField("shell"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let desc = SessionDescriptor {
            uid: 1000,
            gid: 1000,
            home: String::from("/home/alice"),
            shell: String::from("/bin/ion"),
        };
        let line = format_session_descriptor(&desc);
        assert_eq!(line, "uid=1000 gid=1000 home=/home/alice shell=/bin/ion\n");
        let parsed = parse_session_descriptor(&line).unwrap();
        assert_eq!(parsed, desc);
    }

    #[test]
    fn missing_field_errors() {
        let line = "uid=0 gid=0 home=/\n";
        assert_eq!(
            parse_session_descriptor(line),
            Err(ParseError::MissingField("shell"))
        );
    }

    #[test]
    fn bad_uid_errors() {
        let line = "uid=abc gid=0 home=/ shell=/bin/sh\n";
        assert_eq!(
            parse_session_descriptor(line),
            Err(ParseError::BadInteger("uid"))
        );
    }
}
