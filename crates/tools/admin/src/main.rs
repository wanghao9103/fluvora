use std::env;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use fluvora_auth::{Claims, Scopes, TokenKeyRing};

const HELP: &str = "\
Fluvora administration utility

Usage:
  fluvora-admin token --subject <hex-id> [--room <hex-id|*>]
      [--ttl <seconds>] [--scopes <comma-separated>]

The token command reads FLUVORA_TOKEN_SECRETS (active key first) or FLUVORA_TOKEN_SECRET.
Scopes: room_create, room_join, media_publish, room_moderate,
        gift_verify, node_status_write, vod_manage, live_manage, token_revoke, all
";

#[derive(Debug)]
enum AdminError {
    Usage(String),
    MissingSecret,
    WeakSecret,
    InvalidIdentifier,
    InvalidTtl,
    InvalidScope(String),
    Clock,
    Entropy,
    Token,
}

impl fmt::Display for AdminError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => formatter.write_str(message),
            Self::MissingSecret => {
                formatter.write_str("FLUVORA_TOKEN_SECRETS or FLUVORA_TOKEN_SECRET is required")
            }
            Self::WeakSecret => {
                formatter.write_str("every token secret must contain at least 32 bytes")
            }
            Self::InvalidIdentifier => formatter.write_str("subject or room ID is invalid"),
            Self::InvalidTtl => formatter.write_str("TTL must be between 1 and 86400 seconds"),
            Self::InvalidScope(scope) => write!(formatter, "unsupported scope {scope}"),
            Self::Clock => formatter.write_str("system clock is before the Unix epoch"),
            Self::Entropy => formatter.write_str("operating-system entropy is unavailable"),
            Self::Token => formatter.write_str("could not issue access token"),
        }
    }
}

impl std::error::Error for AdminError {}

#[derive(Debug)]
struct TokenOptions {
    subject: u128,
    room_id: u128,
    ttl_seconds: u64,
    scopes: Scopes,
}

fn main() {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match run(&arguments) {
        Ok(Some(output)) => println!("{output}"),
        Ok(None) => {}
        Err(error) => {
            eprintln!("fluvora-admin: {error}");
            std::process::exit(2);
        }
    }
}

fn run(arguments: &[String]) -> Result<Option<String>, AdminError> {
    if arguments.is_empty() || arguments.first().is_some_and(|value| value == "--help") {
        println!("{HELP}");
        return Ok(None);
    }
    if arguments.first().is_none_or(|value| value != "token") {
        return Err(AdminError::Usage(HELP.to_owned()));
    }
    let options = parse_token_options(&arguments[1..])?;
    let secrets = load_token_secrets()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AdminError::Clock)?;
    let now_millis = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
    let expires_at_millis = now_millis
        .checked_add(options.ttl_seconds.saturating_mul(1_000))
        .ok_or(AdminError::InvalidTtl)?;
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| AdminError::Entropy)?;
    TokenKeyRing::new(secrets)
        .map_err(|_| AdminError::WeakSecret)?
        .issue(Claims {
            subject: options.subject,
            room_id: options.room_id,
            expires_at_millis,
            nonce: u64::from_be_bytes(nonce),
            scopes: options.scopes,
        })
        .map(Some)
        .map_err(|_| AdminError::Token)
}

fn load_token_secrets() -> Result<Vec<Vec<u8>>, AdminError> {
    let values = env::var("FLUVORA_TOKEN_SECRETS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map_or_else(|| env::var("FLUVORA_TOKEN_SECRET"), Ok)
        .map_err(|_| AdminError::MissingSecret)?;
    let keys = values
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.as_bytes().to_vec())
        .collect::<Vec<_>>();
    if keys.is_empty() || keys.iter().any(|key| key.len() < 32) {
        Err(AdminError::WeakSecret)
    } else {
        Ok(keys)
    }
}

fn parse_token_options(arguments: &[String]) -> Result<TokenOptions, AdminError> {
    let mut subject = None;
    let mut room_id = 0;
    let mut ttl_seconds = 3_600;
    let mut scopes = Scopes::ROOM_JOIN.union(Scopes::MEDIA_PUBLISH);
    let mut index = 0;
    while index < arguments.len() {
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| AdminError::Usage(format!("missing value for {}", arguments[index])))?;
        match arguments[index].as_str() {
            "--subject" => subject = Some(parse_id(value)?),
            "--room" => room_id = if value == "*" { 0 } else { parse_id(value)? },
            "--ttl" => {
                ttl_seconds = value.parse::<u64>().map_err(|_| AdminError::InvalidTtl)?;
                if !(1..=86_400).contains(&ttl_seconds) {
                    return Err(AdminError::InvalidTtl);
                }
            }
            "--scopes" => scopes = parse_scopes(value)?,
            option => return Err(AdminError::Usage(format!("unsupported option {option}"))),
        }
        index += 2;
    }
    Ok(TokenOptions {
        subject: subject.ok_or_else(|| AdminError::Usage("--subject is required".to_owned()))?,
        room_id,
        ttl_seconds,
        scopes,
    })
}

fn parse_id(value: &str) -> Result<u128, AdminError> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    if value.is_empty() || value.len() > 32 {
        return Err(AdminError::InvalidIdentifier);
    }
    u128::from_str_radix(value, 16).map_err(|_| AdminError::InvalidIdentifier)
}

fn parse_scopes(value: &str) -> Result<Scopes, AdminError> {
    let mut scopes = Scopes::empty();
    for name in value.split(',') {
        let scope = match name {
            "room_create" => Scopes::ROOM_CREATE,
            "room_join" => Scopes::ROOM_JOIN,
            "media_publish" => Scopes::MEDIA_PUBLISH,
            "room_moderate" => Scopes::ROOM_MODERATE,
            "gift_verify" => Scopes::GIFT_VERIFY,
            "node_status_write" => Scopes::NODE_STATUS_WRITE,
            "vod_manage" => Scopes::VOD_MANAGE,
            "live_manage" => Scopes::LIVE_MANAGE,
            "token_revoke" => Scopes::TOKEN_REVOKE,
            "all" => all_scopes(),
            _ => return Err(AdminError::InvalidScope(name.to_owned())),
        };
        scopes = scopes.union(scope);
    }
    Ok(scopes)
}

const fn all_scopes() -> Scopes {
    Scopes::ROOM_CREATE
        .union(Scopes::ROOM_JOIN)
        .union(Scopes::MEDIA_PUBLISH)
        .union(Scopes::ROOM_MODERATE)
        .union(Scopes::GIFT_VERIFY)
        .union(Scopes::NODE_STATUS_WRITE)
        .union(Scopes::VOD_MANAGE)
        .union(Scopes::LIVE_MANAGE)
        .union(Scopes::TOKEN_REVOKE)
}

#[cfg(test)]
mod tests {
    use super::{parse_id, parse_scopes, parse_token_options};
    use fluvora_auth::Scopes;

    #[test]
    fn parses_bounded_token_options() {
        let options = parse_token_options(&[
            "--subject".to_owned(),
            "ab".to_owned(),
            "--room".to_owned(),
            "*".to_owned(),
            "--ttl".to_owned(),
            "60".to_owned(),
            "--scopes".to_owned(),
            "room_join,media_publish".to_owned(),
        ])
        .expect("options");
        assert_eq!(options.subject, 0xab);
        assert_eq!(options.room_id, 0);
        assert_eq!(options.ttl_seconds, 60);
        assert!(options.scopes.contains(Scopes::ROOM_JOIN));
        assert!(options.scopes.contains(Scopes::MEDIA_PUBLISH));
        assert!(parse_id(&"f".repeat(33)).is_err());
        assert!(parse_scopes("root").is_err());
    }
}
