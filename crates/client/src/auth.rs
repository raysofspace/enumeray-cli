//! Native-application authorization code login with S256 PKCE.
use crate::{AuthConfig, Error, OAuthFailureKind, OAuthTokens, validate_url};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{blocking::Client, redirect::Policy};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant, SystemTime},
};
use url::Url;

pub struct LoginAttempt {
    listener: TcpListener,
    state: String,
    verifier: String,
    redirect_uri: String,
    authorization_url: Url,
}

impl LoginAttempt {
    pub fn begin(config: &AuthConfig) -> Result<Self, Error> {
        validate_url(&config.issuer)?;
        validate_url(&config.token_endpoint)?;
        let mut authorization_url = validate_url(&config.authorization_endpoint)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr()?.port()
        );
        let verifier = random_secret()?;
        let state = random_secret()?;
        let challenge = challenge(&verifier);
        authorization_url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", config.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("scope", config.scopes.join(" ").as_str()),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("audience", config.audience.as_str()),
        ]);
        Ok(Self {
            listener,
            state,
            verifier,
            redirect_uri,
            authorization_url,
        })
    }
    pub fn authorization_url(&self) -> &str {
        self.authorization_url.as_str()
    }
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    pub fn finish(self, config: &AuthConfig, timeout: Duration) -> Result<OAuthTokens, Error> {
        self.finish_with_timing(config, timeout)
            .map(|(tokens, _)| tokens)
    }

    /// Return the exchange start alongside tokens so response transit time does
    /// not extend the access token's saved lifetime. Browser waiting is excluded.
    pub fn finish_with_timing(
        self,
        config: &AuthConfig,
        timeout: Duration,
    ) -> Result<(OAuthTokens, SystemTime), Error> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::Invalid("Browser login timeout is too large.".into()))?;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Invalid(
                    "Browser login timed out; start login again.".into(),
                ));
            }
            match self.listener.accept() {
                Ok((mut stream, address)) => {
                    if !address.ip().is_loopback() {
                        continue;
                    }
                    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
                    let code = match callback(&mut stream, &self.state) {
                        Ok(Callback::Code(code)) => code,
                        Ok(Callback::Denied) => {
                            let _ = respond(&mut stream, false);
                            return Err(Error::Invalid("Browser authorization was denied or cancelled; start login again when ready.".into()));
                        }
                        Err(_) => {
                            let _ = respond(&mut stream, false);
                            continue;
                        }
                    };
                    let request = oauth_client()?
                        .post(validate_url(&config.token_endpoint)?)
                        .form(&[
                            ("grant_type", "authorization_code"),
                            ("code", code.as_str()),
                            ("client_id", config.client_id.as_str()),
                            ("redirect_uri", self.redirect_uri.as_str()),
                            ("code_verifier", self.verifier.as_str()),
                        ]);
                    let exchange_started = SystemTime::now();
                    let token = request.send()?;
                    let result = decode_tokens(token).map(|tokens| (tokens, exchange_started));
                    let _ = respond(&mut stream, result.is_ok());
                    return result;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(40))
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

pub fn refresh(config: &AuthConfig, refresh_token: &str) -> Result<OAuthTokens, Error> {
    refresh_until(
        config,
        refresh_token,
        Instant::now() + Duration::from_secs(30),
    )
}

/// Renew once within the caller's remaining workflow budget. Never retry a
/// failed exchange: the identity provider may already have rotated its token.
pub fn refresh_until(
    config: &AuthConfig,
    refresh_token: &str,
    deadline: Instant,
) -> Result<OAuthTokens, Error> {
    let request = oauth_client()?
        .post(validate_url(&config.token_endpoint)?)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", config.client_id.as_str()),
        ]);
    let timeout = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(30));
    if timeout.is_zero() {
        return Err(Error::Invalid(
            "Renewal deadline elapsed before the token exchange.".into(),
        ));
    }
    let response = request.timeout(timeout).send()?;
    decode_tokens(response)
}

fn oauth_client() -> Result<Client, Error> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(Policy::none())
        .build()?)
}

fn decode_tokens(response: reqwest::blocking::Response) -> Result<OAuthTokens, Error> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let failure = |kind| Error::OAuth {
        status: status.as_u16(),
        kind,
        retry_after: retry_after.clone(),
    };
    let mut bytes = Vec::new();
    response.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(failure(OAuthFailureKind::InvalidResponse));
    }
    if !status.is_success() {
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| failure(OAuthFailureKind::InvalidResponse))?;
        let kind = if status.is_client_error() || status.is_server_error() {
            match document.get("error").and_then(serde_json::Value::as_str) {
                Some("invalid_grant") => OAuthFailureKind::InvalidGrant,
                Some(
                    "invalid_client"
                    | "unauthorized_client"
                    | "invalid_request"
                    | "unsupported_grant_type"
                    | "invalid_scope",
                ) => OAuthFailureKind::Configuration,
                Some("temporarily_unavailable" | "server_error") => {
                    OAuthFailureKind::TemporarilyUnavailable
                }
                _ => OAuthFailureKind::InvalidResponse,
            }
        } else {
            OAuthFailureKind::InvalidResponse
        };
        return Err(failure(kind));
    }
    let tokens: OAuthTokens =
        serde_json::from_slice(&bytes).map_err(|_| failure(OAuthFailureKind::InvalidResponse))?;
    let valid_secret = |value: &str| !value.is_empty() && !value.chars().any(char::is_whitespace);
    if !tokens.token_type.eq_ignore_ascii_case("bearer")
        || !valid_secret(&tokens.access_token)
        || tokens
            .refresh_token
            .as_deref()
            .is_some_and(|value| !valid_secret(value))
        || tokens.expires_in == Some(0)
    {
        return Err(failure(OAuthFailureKind::InvalidResponse));
    }
    Ok(tokens)
}

fn random_secret() -> Result<String, Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| Error::Invalid("Operating-system random source is unavailable.".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[derive(Debug, PartialEq, Eq)]
enum Callback {
    Code(String),
    Denied,
}

fn callback(stream: &mut TcpStream, state: &str) -> Result<Callback, Error> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8];
    while bytes.len() < 8192 {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&bytes)
        .map_err(|_| Error::Invalid("Malformed OAuth callback.".into()))?;
    let mut parts = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    if parts.next() != Some("GET") {
        return Err(Error::Invalid("Unexpected OAuth callback method.".into()));
    }
    parse_callback(parts.next().unwrap_or_default(), state)
}

fn parse_callback(path: &str, state: &str) -> Result<Callback, Error> {
    let invalid = || Error::Invalid("Invalid or unsolicited OAuth callback.".into());
    if !path.starts_with("/callback?") {
        return Err(invalid());
    }
    let url = Url::parse(&format!("http://127.0.0.1{path}")).map_err(|_| invalid())?;
    let mut found_state = None;
    let mut code = None;
    let mut denied = None;
    for (name, value) in url.query_pairs() {
        match name.as_ref() {
            "state" if found_state.is_none() => found_state = Some(value.into_owned()),
            "code" if code.is_none() => code = Some(value.into_owned()),
            "error" if denied.is_none() => denied = Some(value.into_owned()),
            "state" | "code" | "error" => return Err(invalid()),
            _ => {}
        }
    }
    if found_state.as_deref() != Some(state) {
        return Err(invalid());
    }
    match (code, denied) {
        (Some(code), None) if !code.is_empty() => Ok(Callback::Code(code)),
        (None, Some(error)) if !error.is_empty() => Ok(Callback::Denied),
        _ => Err(invalid()),
    }
}

fn respond(stream: &mut TcpStream, success: bool) -> std::io::Result<()> {
    let body = if success {
        "Enumeray login completed. You can close this window."
    } else {
        "Enumeray login could not be completed. Return to the terminal."
    };
    let status = if success { "200 OK" } else { "400 Bad Request" };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_matches_rfc7636_vector() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
    #[test]
    fn callback_rejects_cross_site_duplicate_or_error_responses() {
        assert_eq!(
            parse_callback("/callback?state=s&code=c", "s").unwrap(),
            Callback::Code("c".into())
        );
        assert_eq!(
            parse_callback("/callback?state=s&code=c&session_state=session&iss=https%3A%2F%2Fid.example%2Frealms%2Fenumeray", "s").unwrap(),
            Callback::Code("c".into())
        );
        assert_eq!(
            parse_callback(
                "/callback?state=s&error=access_denied&error_description=private",
                "s"
            )
            .unwrap(),
            Callback::Denied
        );
        for path in [
            "/callback?state=other&code=c",
            "/callback?state=s&code=a&code=b",
            "/callback?state=s&code=c&error=denied",
            "/callback?state=other&error=access_denied",
            "/callback?state=s&error=denied&error=again",
            "/callback?state=s&error=",
            "/other?state=s&code=c",
            "https://elsewhere/callback?state=s&code=c",
        ] {
            assert!(parse_callback(path, "s").is_err());
        }
    }

    #[test]
    fn native_code_exchange_rejects_unsolicited_callback_and_refreshes_without_a_secret() {
        let token_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", token_listener.local_addr().unwrap());
        let config = AuthConfig {
            issuer: format!("{origin}/realms/enumeray"),
            client_id: "enumeray-cli".into(),
            audience: "enumeray-api".into(),
            authorization_endpoint: format!("{origin}/authorize"),
            token_endpoint: format!("{origin}/token"),
            scopes: vec!["openid".into(), "offline_access".into()],
        };
        let attempt = LoginAttempt::begin(&config).unwrap();
        let query: std::collections::HashMap<_, _> = attempt
            .authorization_url
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(attempt.listener.local_addr().unwrap().ip().is_loopback());
        let redirect = attempt.redirect_uri().to_owned();
        let state = query["state"].clone();
        let token_worker = std::thread::spawn(move || {
            let mut forms = Vec::new();
            let mut first_post_received = None;
            for refresh in ["first-refresh", "rotated-refresh"] {
                let (mut stream, _) = token_listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut byte = [0];
                while !bytes.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                forms.push(
                    url::form_urlencoded::parse(&body)
                        .into_owned()
                        .collect::<std::collections::HashMap<_, _>>(),
                );
                if first_post_received.is_none() {
                    first_post_received = Some(SystemTime::now());
                    std::thread::sleep(Duration::from_millis(40));
                }
                let body = serde_json::json!({"access_token":"access", "token_type":"Bearer",
                    "refresh_token":refresh,"expires_in":300})
                .to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            }
            (forms, first_post_received.unwrap())
        });
        let callback_worker = std::thread::spawn(move || {
            let client = oauth_client().unwrap();
            assert_eq!(
                client
                    .get(format!("{redirect}?state=wrong&code=unsolicited"))
                    .send()
                    .unwrap()
                    .status(),
                400
            );
            std::thread::sleep(Duration::from_millis(40));
            let accepted_callback_sent = SystemTime::now();
            assert_eq!(
                client
                    .get(format!(
                        "{redirect}?state={state}&code=accepted&session_state=session&iss=provider"
                    ))
                    .send()
                    .unwrap()
                    .status(),
                200
            );
            accepted_callback_sent
        });
        let (tokens, exchange_started) = attempt
            .finish_with_timing(&config, Duration::from_secs(10))
            .unwrap();
        let callback_sent = callback_worker.join().unwrap();
        let renewed = refresh(&config, tokens.refresh_token.as_deref().unwrap()).unwrap();
        assert_eq!(renewed.refresh_token.as_deref(), Some("rotated-refresh"));
        let (forms, first_post_received) = token_worker.join().unwrap();
        assert!(
            exchange_started >= callback_sent,
            "browser waiting must not consume the token lifetime"
        );
        assert!(
            exchange_started <= first_post_received,
            "response transit must not extend the token lifetime"
        );
        assert_eq!(forms[0]["code"], "accepted");
        assert_eq!(forms[0]["redirect_uri"], query["redirect_uri"]);
        assert_eq!(
            challenge(&forms[0]["code_verifier"]),
            query["code_challenge"]
        );
        assert_eq!(forms[1]["grant_type"], "refresh_token");
        assert_eq!(forms[1]["refresh_token"], "first-refresh");
        for form in forms {
            assert_eq!(form["client_id"], "enumeray-cli");
            assert!(!form.contains_key("client_secret"));
        }
    }

    #[test]
    fn verified_browser_denial_finishes_without_waiting_or_exchanging_a_token() {
        let config = AuthConfig {
            issuer: "http://127.0.0.1:1".into(),
            client_id: "native".into(),
            audience: "enumeray".into(),
            authorization_endpoint: "http://127.0.0.1:1/authorize".into(),
            token_endpoint: "http://127.0.0.1:1/token".into(),
            scopes: vec![],
        };
        let attempt = LoginAttempt::begin(&config).unwrap();
        let state = attempt
            .authorization_url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let address = attempt.listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            write!(stream, "GET /callback?state={state}&error=access_denied HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            assert!(reply.starts_with("HTTP/1.1 400"));
        });
        let started = Instant::now();
        assert!(
            matches!(attempt.finish(&config, Duration::from_secs(5)), Err(Error::Invalid(message)) if message.contains("denied or cancelled"))
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        worker.join().unwrap();
    }

    fn token_reply(
        status: u16,
        body: String,
        delay: Duration,
    ) -> (AuthConfig, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config = AuthConfig {
            issuer: origin.clone(),
            client_id: "native".into(),
            audience: "api".into(),
            authorization_endpoint: format!("{origin}/authorize"),
            token_endpoint: format!("{origin}/token"),
            scopes: vec![],
        };
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
            }
            let header = String::from_utf8(bytes).unwrap();
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut request = vec![0; length];
            stream.read_exact(&mut request).unwrap();
            std::thread::sleep(delay);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} Response\r\nConnection: close\r\nContent-Type: application/json\r\nRetry-After: 3\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
        });
        (config, worker)
    }

    #[test]
    fn oauth_failures_are_classified_without_exposing_provider_descriptions_or_secrets() {
        for (status, body, expected) in [
            (
                400,
                r#"{"error":"invalid_grant","error_description":"private-refresh"}"#.into(),
                OAuthFailureKind::InvalidGrant,
            ),
            (
                401,
                r#"{"error":"invalid_client","error_description":"private-refresh"}"#.into(),
                OAuthFailureKind::Configuration,
            ),
            (
                503,
                r#"{"error":"temporarily_unavailable","error_description":"private-refresh"}"#
                    .into(),
                OAuthFailureKind::TemporarilyUnavailable,
            ),
            (
                500,
                r#"{"error":"server_error","error_description":"private-refresh"}"#.into(),
                OAuthFailureKind::TemporarilyUnavailable,
            ),
            (
                503,
                "private-refresh proxy reply".into(),
                OAuthFailureKind::InvalidResponse,
            ),
            (
                400,
                r#"{"error":"private-refresh"}"#.into(),
                OAuthFailureKind::InvalidResponse,
            ),
            (
                302,
                r#"{"error":"invalid_grant"}"#.into(),
                OAuthFailureKind::InvalidResponse,
            ),
            (
                200,
                r#"{"access_token":"private-refresh","token_type":"invalid"}"#.into(),
                OAuthFailureKind::InvalidResponse,
            ),
            (
                200,
                "x".repeat(64 * 1024 + 1),
                OAuthFailureKind::InvalidResponse,
            ),
        ] {
            let (config, worker) = token_reply(status, body, Duration::ZERO);
            let error = refresh(&config, "private-refresh")
                .err()
                .expect("exchange must fail");
            assert!(!error.to_string().contains("private-refresh"));
            assert!(!format!("{error:?}").contains("private-refresh"));
            assert!(
                matches!(error, Error::OAuth { status: actual, kind, retry_after: Some(ref retry) } if actual == status && kind == expected && retry == "3")
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn refresh_until_uses_the_remaining_deadline_and_does_not_retry() {
        let (config, worker) = token_reply(200, r#"{"access_token":"access","token_type":"Bearer","refresh_token":"rotated","expires_in":300}"#.into(), Duration::from_millis(150));
        let started = Instant::now();
        let error = refresh_until(&config, "original", started + Duration::from_millis(40))
            .err()
            .expect("deadline must expire");
        assert!(matches!(error, Error::Transport(ref failure) if failure.is_timeout()));
        assert!(started.elapsed() < Duration::from_millis(140));
        worker.join().unwrap();
        assert!(matches!(
            refresh_until(&config, "original", Instant::now()),
            Err(Error::Invalid(_))
        ));
    }
}
