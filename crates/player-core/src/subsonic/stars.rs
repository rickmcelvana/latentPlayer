//! Stars and ratings (Phase 2 M4): the listener's own marks on someone else's
//! library.
//!
//! Small, and one detail in it is the reason it is its own module rather than
//! three lines somewhere else. `star`/`unstar` take **three different id
//! parameters**, and which one to use depends on what is being starred:
//!
//! - `id` -- a song, or an album/artist as the *folder-based* API names them.
//! - `albumId` -- an album, as the ID3-tag-based API names them.
//! - `artistId` -- an artist, likewise.
//!
//! Every browse request this app makes is the ID3-tagged kind (`getArtists`,
//! `getAlbumList2`, `getAlbum` -- see `browse.rs`), so an album id sent as `id`
//! is being named the way a different API names a different thing. The server
//! finds nothing under it and answers `"status": "ok"`: the star simply does not
//! happen, with no error anywhere to notice. That is why the kind is part of
//! [`StarTarget`] rather than something a caller remembers to pass, and why
//! `tests/subsonic_write.rs` asks a fake server which parameter actually
//! arrived (decision 12 in tasks/phase-2.md).

use serde::{Deserialize, Serialize};

use super::{Parsed, SubsonicError};

/// Highest rating `setRating` accepts. 0 is not a rating -- it clears one.
pub const MAX_RATING: u32 = 5;

/// What is being starred, which decides how its id is spelled on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StarTarget {
    Song,
    Album,
    Artist,
}

impl StarTarget {
    /// The parameter name this kind's id goes in -- see the module docs for what
    /// getting it wrong looks like (nothing, quietly).
    fn param(self) -> &'static str {
        match self {
            Self::Song => "id",
            Self::Album => "albumId",
            Self::Artist => "artistId",
        }
    }
}

/// One mark to put on, or take off, something in the server's library.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MarkRequest {
    Star {
        target: StarTarget,
        id: String,
    },
    Unstar {
        target: StarTarget,
        id: String,
    },
    /// 1-5, or 0 to clear the rating. Values above [`MAX_RATING`] are clamped
    /// rather than sent: the server answers an out-of-range rating with error
    /// 10, which would read to the listener as "rating is broken".
    ///
    /// Note `setRating` has only one id parameter, unlike star -- so the kind is
    /// not part of this variant. That asymmetry is the endpoint's, not this
    /// module's.
    SetRating {
        id: String,
        rating: u32,
    },
}

impl MarkRequest {
    pub fn endpoint(&self) -> &'static str {
        match self {
            Self::Star { .. } => "star",
            Self::Unstar { .. } => "unstar",
            Self::SetRating { .. } => "setRating",
        }
    }

    pub fn params(&self) -> Vec<(String, String)> {
        match self {
            Self::Star { target, id } | Self::Unstar { target, id } => {
                vec![(target.param().into(), id.clone())]
            }
            Self::SetRating { id, rating } => vec![
                ("id".into(), id.clone()),
                ("rating".into(), rating.min(&MAX_RATING).to_string()),
            ],
        }
    }
}

/// Reads the response to a mark.
///
/// All three endpoints answer with nothing but success, so there is no payload
/// to hand back -- what this checks is the envelope, which is the only place a
/// refusal appears (a read-only server, a user without permission, an id the
/// server does not recognise).
pub fn parse_mark(bytes: &[u8]) -> Result<Parsed<()>, SubsonicError> {
    let parsed = super::parse::<super::NoPayload>(bytes)?;
    Ok(Parsed {
        info: parsed.info,
        payload: (),
    })
}

/// Whether the server says this item is starred, from the `starred` field.
///
/// The field is a **date string** -- when it was starred -- and it is absent
/// when it is not. So the boolean the UI draws is "is there a date here", and
/// this is the one place that conversion happens (decision 13).
pub fn is_starred(starred: Option<&str>) -> bool {
    starred.is_some_and(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of this module, as a test.
    #[test]
    fn each_kind_of_star_names_its_id_differently() {
        assert_eq!(
            MarkRequest::Star {
                target: StarTarget::Song,
                id: "s1".into()
            }
            .params(),
            vec![("id".to_string(), "s1".to_string())]
        );
        assert_eq!(
            MarkRequest::Star {
                target: StarTarget::Album,
                id: "al1".into()
            }
            .params(),
            vec![("albumId".to_string(), "al1".to_string())]
        );
        assert_eq!(
            MarkRequest::Star {
                target: StarTarget::Artist,
                id: "ar1".into()
            }
            .params(),
            vec![("artistId".to_string(), "ar1".to_string())]
        );
    }

    #[test]
    fn unstarring_is_the_same_request_at_a_different_endpoint() {
        let star = MarkRequest::Star {
            target: StarTarget::Album,
            id: "al1".into(),
        };
        let unstar = MarkRequest::Unstar {
            target: StarTarget::Album,
            id: "al1".into(),
        };
        assert_eq!(star.endpoint(), "star");
        assert_eq!(unstar.endpoint(), "unstar");
        assert_eq!(star.params(), unstar.params());
    }

    #[test]
    fn a_rating_above_five_is_clamped_rather_than_refused_by_the_server() {
        let params = MarkRequest::SetRating {
            id: "s1".into(),
            rating: 9,
        }
        .params();
        assert_eq!(params[1], ("rating".to_string(), "5".to_string()));
    }

    #[test]
    fn zero_clears_a_rating_and_is_left_alone() {
        let params = MarkRequest::SetRating {
            id: "s1".into(),
            rating: 0,
        }
        .params();
        assert_eq!(params[1], ("rating".to_string(), "0".to_string()));
    }

    #[test]
    fn starred_is_a_date_and_not_a_boolean() {
        assert!(is_starred(Some("2026-08-18T04:00:00.000Z")));
        assert!(!is_starred(None));
        // An empty string is not a date, and a server that sends one is saying
        // nothing rather than saying yes.
        assert!(!is_starred(Some("")));
    }

    #[test]
    fn a_refused_mark_is_an_error_inside_an_http_200() {
        let body = br#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":70,"message":"not found"}}}"#;
        let error = parse_mark(body).expect_err("a failure envelope must not read as success");
        assert!(matches!(
            error,
            SubsonicError::Server {
                code: super::super::ErrorCode::NotFound,
                ..
            }
        ));
    }

    #[test]
    fn a_successful_mark_answers_with_nothing() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
        let parsed = parse_mark(body).expect("an empty success should parse");
        assert_eq!(parsed.info.version, "1.16.1");
    }
}
