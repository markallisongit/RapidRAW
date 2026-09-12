//! OAuth 1.0a (RFC 5849) request signing, HMAC-SHA1 only.
//!
//! This module is deliberately service-agnostic: it knows nothing about any
//! particular API. The nonce and timestamp are caller-supplied so signatures
//! are reproducible in tests against published vectors.

use std::fmt::Write as _;

use base64::{Engine as _, engine::general_purpose};
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

/// RFC 3986 unreserved-only percent-encoding: `A-Za-z0-9-._~` pass through,
/// every other byte becomes `%` plus uppercase hex. Space is `%20`, never `+`.
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Client and (optionally) token credentials for a single signed request.
///
/// `token`/`token_secret` are absent for a temporary-credentials request and
/// present once the client holds a request or access token.
#[derive(Clone, Debug)]
pub struct Credentials {
    pub consumer_key: String,
    pub consumer_secret: String,
    pub token: Option<String>,
    pub token_secret: Option<String>,
}

/// RFC 5849 section 3.4.1: `METHOD&encoded_url&encoded_normalized_params`.
///
/// `url` must already be normalized: scheme and host lowercased, default port
/// removed, and no query string or fragment — any query parameters belong in
/// `params`. Parameters are sorted by encoded key and then by encoded value, so
/// repeated keys keep a defined order. `oauth_signature` must never be present.
pub fn signature_base_string(method: &str, url: &str, params: &[(String, String)]) -> String {
    let mut encoded: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (percent_encode(k), percent_encode(v)))
        .collect();
    encoded.sort();

    let normalized = encoded
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    format!(
        "{}&{}&{}",
        method.to_ascii_uppercase(),
        percent_encode(url),
        percent_encode(&normalized)
    )
}

/// HMAC-SHA1 over `base_string`, keyed by the two percent-encoded secrets
/// joined with `&`, returned base64-encoded. A missing token secret still
/// contributes its separator, leaving a key that ends in `&`.
pub fn sign_hmac_sha1(
    base_string: &str,
    consumer_secret: &str,
    token_secret: Option<&str>,
) -> String {
    let key = format!(
        "{}&{}",
        percent_encode(consumer_secret),
        percent_encode(token_secret.unwrap_or(""))
    );
    let mut mac =
        Hmac::<Sha1>::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(base_string.as_bytes());
    general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Builds the `Authorization` header value for a signed OAuth 1.0a request.
///
/// `extra_params` carries the request's query-string and form-encoded
/// parameters; they are signed but, per RFC 5849 section 3.5.1, only `oauth_*`
/// parameters are echoed in the header. A query string on `url` is split off
/// and signed as well, so callers may pass a whole request URL.
///
/// `nonce` and `timestamp` are supplied by the caller rather than generated
/// here so that signatures are reproducible against published test vectors.
pub fn authorization_header(
    method: &str,
    url: &str,
    extra_params: &[(String, String)],
    creds: &Credentials,
    nonce: &str,
    timestamp: u64,
) -> String {
    let (base_url, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };

    let mut params: Vec<(String, String)> = vec![
        ("oauth_consumer_key".into(), creds.consumer_key.clone()),
        ("oauth_nonce".into(), nonce.to_string()),
        ("oauth_signature_method".into(), "HMAC-SHA1".into()),
        ("oauth_timestamp".into(), timestamp.to_string()),
        ("oauth_version".into(), "1.0".into()),
    ];
    if let Some(token) = &creds.token {
        params.push(("oauth_token".into(), token.clone()));
    }
    params.extend(query.into_iter().flat_map(parse_query));
    params.extend(extra_params.iter().cloned());

    let base = signature_base_string(method, base_url, &params);
    let signature = sign_hmac_sha1(&base, &creds.consumer_secret, creds.token_secret.as_deref());
    params.push(("oauth_signature".into(), signature));

    let mut protocol: Vec<&(String, String)> = params
        .iter()
        .filter(|(k, _)| k.starts_with("oauth_"))
        .collect();
    protocol.sort();

    let pairs = protocol
        .iter()
        .map(|(k, v)| format!("{}=\"{}\"", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join(", ");

    format!("OAuth {pairs}")
}

/// Splits an `a=1&b=2` query string into decoded key/value pairs, which
/// `signature_base_string` then re-encodes. A key with no `=` keeps an empty
/// value, per RFC 5849 section 3.4.1.3.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (percent_decode(key), percent_decode(value)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

/// Reverses `percent_encode`. A malformed escape is passed through unchanged,
/// and `+` decodes to a space to match form encoding.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&input[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn percent_encoding_matches_rfc3986_unreserved() {
        let cases = [
            ("abcABC123", "abcABC123"),
            ("-._~", "-._~"),
            ("%", "%25"),
            ("+", "%2B"),
            (" ", "%20"),
            ("&", "%26"),
            ("=", "%3D"),
            ("/", "%2F"),
            ("Ladies + Gentlemen", "Ladies%20%2B%20Gentlemen"),
            ("An ++lingéate", "An%20%2B%2Bling%C3%A9ate"),
            ("Dogs, Cats & Mice", "Dogs%2C%20Cats%20%26%20Mice"),
            ("\u{2603}", "%E2%98%83"),
        ];
        for (input, expected) in cases {
            assert_eq!(percent_encode(input), expected, "input: {input:?}");
        }
    }

    /// RFC 5849 section 3.4.1.1 prints this base string verbatim. It is the
    /// strongest available check on parameter normalisation: a repeated key
    /// (`a3`) that must sort by value, empty values, and a value containing an
    /// already-percent-encoded sequence that must be encoded a second time.
    #[test]
    fn signature_base_string_matches_rfc5849_section_3_4_1_1() {
        let params = params(&[
            ("b5", "=%3D"),
            ("a3", "a"),
            ("c@", ""),
            ("a2", "r b"),
            ("oauth_consumer_key", "9djdj82h48djs9d2"),
            ("oauth_token", "kkk9d7dh3k39sjv7"),
            ("oauth_signature_method", "HMAC-SHA1"),
            ("oauth_timestamp", "137131201"),
            ("oauth_nonce", "7d8f3e4a"),
            ("c2", ""),
            ("a3", "2 q"),
        ]);

        assert_eq!(
            signature_base_string("POST", "http://example.com/request", &params),
            "POST&http%3A%2F%2Fexample.com%2Frequest&a2%3Dr%2520b%26a3%3D2%2520q\
%26a3%3Da%26b5%3D%253D%25253D%26c%2540%3D%26c2%3D%26oauth_consumer_\
key%3D9djdj82h48djs9d2%26oauth_nonce%3D7d8f3e4a%26oauth_signature_m\
ethod%3DHMAC-SHA1%26oauth_timestamp%3D137131201%26oauth_token%3Dkkk\
9d7dh3k39sjv7"
        );
    }

    /// RFC 5849 section 1.2, third request: accessing a protected resource with
    /// both a client and a token secret.
    #[test]
    fn rfc5849_section_1_2_protected_resource_signature() {
        let params = params(&[
            ("file", "vacation.jpg"),
            ("size", "original"),
            ("oauth_consumer_key", "dpf43f3p2l4k3l03"),
            ("oauth_token", "nnch734d00sl2jdk"),
            ("oauth_signature_method", "HMAC-SHA1"),
            ("oauth_timestamp", "137131202"),
            ("oauth_nonce", "chapoH"),
        ]);

        let base = signature_base_string("GET", "http://photos.example.net/photos", &params);
        assert_eq!(
            base,
            "GET&http%3A%2F%2Fphotos.example.net%2Fphotos&file%3Dvacation.jpg\
%26oauth_consumer_key%3Ddpf43f3p2l4k3l03%26oauth_nonce%3DchapoH\
%26oauth_signature_method%3DHMAC-SHA1%26oauth_timestamp%3D137131202\
%26oauth_token%3Dnnch734d00sl2jdk%26size%3Doriginal"
        );
        assert_eq!(
            sign_hmac_sha1(&base, "kd94hf93k423kf44", Some("pfkkdhi9sl3r4s00")),
            "MdpQcU8iPSUjWoN/UDMsK2sui9I="
        );
    }

    /// RFC 5849 section 1.2, first request: temporary credentials, so there is
    /// no token secret and the signing key ends in a bare `&`.
    #[test]
    fn rfc5849_section_1_2_temporary_credentials_signature_has_empty_token_secret() {
        let params = params(&[
            ("oauth_consumer_key", "dpf43f3p2l4k3l03"),
            ("oauth_signature_method", "HMAC-SHA1"),
            ("oauth_timestamp", "137131200"),
            ("oauth_nonce", "wIjqoS"),
            ("oauth_callback", "http://printer.example.com/ready"),
        ]);

        let base = signature_base_string("POST", "https://photos.example.net/initiate", &params);
        assert_eq!(
            sign_hmac_sha1(&base, "kd94hf93k423kf44", None),
            "74KNZJeDHnMBp0EMJ9ZHt/XKycU="
        );
    }

    /// OAuth Core 1.0 appendix A.5.1, which unlike the RFC 5849 examples does
    /// carry `oauth_version`, so it exercises `authorization_header` end to end.
    #[test]
    fn authorization_header_matches_oauth_core_1_0_appendix_a_5_1() {
        let creds = Credentials {
            consumer_key: "dpf43f3p2l4k3l03".into(),
            consumer_secret: "kd94hf93k423kf44".into(),
            token: Some("nnch734d00sl2jdk".into()),
            token_secret: Some("pfkkdhi9sl3r4s00".into()),
        };

        let header = authorization_header(
            "GET",
            "http://photos.example.net/photos",
            &params(&[("file", "vacation.jpg"), ("size", "original")]),
            &creds,
            "kllo9940pd9333jh",
            1191242096,
        );

        assert_eq!(
            header,
            concat!(
                "OAuth ",
                "oauth_consumer_key=\"dpf43f3p2l4k3l03\", ",
                "oauth_nonce=\"kllo9940pd9333jh\", ",
                "oauth_signature=\"tR3%2BTy81lMeYAr%2FFid0kMTYa%2FWM%3D\", ",
                "oauth_signature_method=\"HMAC-SHA1\", ",
                "oauth_timestamp=\"1191242096\", ",
                "oauth_token=\"nnch734d00sl2jdk\", ",
                "oauth_version=\"1.0\""
            )
        );
    }

    /// Non-protocol parameters are signed but must not be echoed in the header.
    #[test]
    fn authorization_header_omits_non_oauth_parameters() {
        let creds = Credentials {
            consumer_key: "key".into(),
            consumer_secret: "secret".into(),
            token: None,
            token_secret: None,
        };

        let header = authorization_header(
            "GET",
            "http://example.com/r",
            &params(&[("file", "vacation.jpg")]),
            &creds,
            "nonce",
            1,
        );

        assert!(
            !header.contains("file"),
            "header leaked a query parameter: {header}"
        );
        assert!(
            !header.contains("oauth_token"),
            "header invented a token: {header}"
        );
    }

    // --- SmugMug vectors -----------------------------------------------
    //
    // SmugMug's example code (gist 10046914) contains no hardcoded vectors --
    // it delegates signing to Python's `rauth`. These signatures were therefore
    // generated by running `rauth` 0.7.3 itself over SmugMug-shaped requests,
    // using the endpoints and parameters from that gist's `common.py` and
    // `console.py`. The credentials are invented; nothing here touches network.

    const SMUGMUG_KEY: &str = "ExampleConsumerKeyABCDEF";
    const SMUGMUG_SECRET: &str = "ExampleConsumerSecret0123456789abcdef";
    const SMUGMUG_TOKEN: &str = "ExampleAccessToken123456";
    const SMUGMUG_TOKEN_SECRET: &str = "ExampleAccessTokenSecret0123456789ab";
    const SMUGMUG_NONCE: &str = "7bcbc1e5e4a24c8da9b0d1a2c3d4e5f6";
    const SMUGMUG_TIMESTAMP: u64 = 1749000000;

    /// Pulls the `oauth_signature` value back out of a header, percent-decoded,
    /// so it can be compared against a raw base64 signature.
    fn signature_from(header: &str) -> String {
        let start = header
            .find("oauth_signature=\"")
            .expect("header carries a signature")
            + "oauth_signature=\"".len();
        let value = &header[start..][..header[start..].find('"').expect("closing quote")];
        value
            .replace("%2B", "+")
            .replace("%2F", "/")
            .replace("%3D", "=")
    }

    /// The temporary-credentials request: no token at all, and the out-of-band
    /// callback SmugMug's console example uses.
    #[test]
    fn smugmug_get_request_token_signature() {
        let creds = Credentials {
            consumer_key: SMUGMUG_KEY.into(),
            consumer_secret: SMUGMUG_SECRET.into(),
            token: None,
            token_secret: None,
        };

        let header = authorization_header(
            "POST",
            "https://secure.smugmug.com/services/oauth/1.0a/getRequestToken",
            &params(&[("oauth_callback", "oob")]),
            &creds,
            SMUGMUG_NONCE,
            SMUGMUG_TIMESTAMP,
        );

        assert_eq!(signature_from(&header), "XQfLNtH4vNC9MAnV2TkX9n4KbQA=");
        assert!(
            header.contains("oauth_callback=\"oob\""),
            "oauth_callback must be echoed in the header: {header}"
        );
    }

    /// Exchanging a request token plus the six-digit verifier for an access
    /// token: signed with the *request* token secret.
    #[test]
    fn smugmug_get_access_token_signature() {
        let creds = Credentials {
            consumer_key: SMUGMUG_KEY.into(),
            consumer_secret: SMUGMUG_SECRET.into(),
            token: Some("ExampleRequestToken98765".into()),
            token_secret: Some("ExampleRequestTokenSecret0123456789".into()),
        };

        let header = authorization_header(
            "POST",
            "https://secure.smugmug.com/services/oauth/1.0a/getAccessToken",
            &params(&[("oauth_verifier", "123456")]),
            &creds,
            SMUGMUG_NONCE,
            SMUGMUG_TIMESTAMP,
        );

        assert_eq!(signature_from(&header), "vSjxniP/OGCy9tF1PK8oVQhaz9A=");
        assert!(
            header.contains("oauth_verifier=\"123456\""),
            "oauth_verifier must be echoed in the header: {header}"
        );
    }

    /// A protected-resource call. SmugMug's API puts `!` in its paths, which is
    /// reserved under RFC 3986 and so must be percent-encoded in the base
    /// string, and `_pretty` is an empty-valued parameter that must still be
    /// signed.
    #[test]
    fn smugmug_authuser_signature_encodes_bang_and_empty_valued_param() {
        let creds = Credentials {
            consumer_key: SMUGMUG_KEY.into(),
            consumer_secret: SMUGMUG_SECRET.into(),
            token: Some(SMUGMUG_TOKEN.into()),
            token_secret: Some(SMUGMUG_TOKEN_SECRET.into()),
        };

        let header = authorization_header(
            "GET",
            "https://api.smugmug.com/api/v2!authuser",
            &params(&[("_pretty", "")]),
            &creds,
            SMUGMUG_NONCE,
            SMUGMUG_TIMESTAMP,
        );

        assert_eq!(signature_from(&header), "GSsOr/l2aHCvzfZ4g3DLp07u3Fs=");
    }

    /// Callers should be able to hand over a whole request URL; the query
    /// string is part of the signature, not part of the signed URL.
    #[test]
    fn smugmug_query_string_on_the_url_is_signed() {
        let creds = Credentials {
            consumer_key: SMUGMUG_KEY.into(),
            consumer_secret: SMUGMUG_SECRET.into(),
            token: Some(SMUGMUG_TOKEN.into()),
            token_secret: Some(SMUGMUG_TOKEN_SECRET.into()),
        };

        let header = authorization_header(
            "GET",
            "https://api.smugmug.com/api/v2/album/AbCdEf!images?count=5&start=1",
            &[],
            &creds,
            SMUGMUG_NONCE,
            SMUGMUG_TIMESTAMP,
        );

        assert_eq!(signature_from(&header), "bqa5IJsc+CRUP/FV1DYccTEGjrI=");
    }
}
