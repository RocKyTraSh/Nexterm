//! Minimal SOCKS5 server-side handling for dynamic (`ssh -D`) forwarding.
//!
//! Scope (RFC 1928, server side, just enough for `ssh -D`):
//! * SOCKS5 only (no SOCKS4/4a);
//! * `NO AUTHENTICATION REQUIRED` (`0x00`) only — no username/password auth;
//! * `CONNECT` (`0x01`) only — no `BIND`, no `UDP ASSOCIATE`;
//! * address types IPv4 / domain name / IPv6.
//!
//! The byte-level parsing/encoding lives in pure functions ([`select_method`],
//! [`parse_request`], [`encode_reply`]) that are unit-tested without any I/O.
//! The async helpers ([`negotiate_method`], [`read_request`], [`write_reply`])
//! drive those over any `AsyncRead + AsyncWrite` stream with bounded `read_exact`
//! calls (never an unbounded read). Nothing here depends on `russh`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// SOCKS protocol version we speak.
pub const SOCKS5_VERSION: u8 = 0x05;
/// Auth method: no authentication required.
pub const METHOD_NO_AUTH: u8 = 0x00;
/// Auth method sentinel: no acceptable methods.
pub const METHOD_NO_ACCEPTABLE: u8 = 0xFF;
/// Command: establish a TCP stream (the only one we support).
pub const CMD_CONNECT: u8 = 0x01;

const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

/// Maximum domain name length (a SOCKS5 domain length field is a single byte).
pub const MAX_DOMAIN_LEN: usize = 255;

/// Errors from SOCKS5 negotiation / parsing.
#[derive(Debug, Error)]
pub enum Socks5Error {
    #[error("unsupported SOCKS version: {0:#04x} (only SOCKS5 is supported)")]
    BadVersion(u8),
    #[error("no acceptable authentication methods (only NO AUTH is supported)")]
    NoAcceptableMethods,
    #[error("unsupported SOCKS command: {0:#04x} (only CONNECT is supported)")]
    UnsupportedCommand(u8),
    #[error("unsupported address type: {0:#04x}")]
    UnsupportedAddressType(u8),
    #[error("malformed SOCKS message: {0}")]
    Malformed(&'static str),
    #[error("socks i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// SOCKS5 reply codes (RFC 1928 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ReplyCode {
    Succeeded = 0x00,
    GeneralFailure = 0x01,
    ConnectionNotAllowed = 0x02,
    NetworkUnreachable = 0x03,
    HostUnreachable = 0x04,
    ConnectionRefused = 0x05,
    TtlExpired = 0x06,
    CommandNotSupported = 0x07,
    AddressTypeNotSupported = 0x08,
}

impl Socks5Error {
    /// The reply code to send back for this error (when still at a stage where a
    /// CONNECT reply is meaningful).
    pub fn reply_code(&self) -> ReplyCode {
        match self {
            Socks5Error::UnsupportedCommand(_) => ReplyCode::CommandNotSupported,
            Socks5Error::UnsupportedAddressType(_) => ReplyCode::AddressTypeNotSupported,
            _ => ReplyCode::GeneralFailure,
        }
    }
}

/// A destination address from a CONNECT request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetAddr {
    Ip(IpAddr),
    Domain(String),
}

impl TargetAddr {
    /// Host string suitable for `direct-tcpip` (IP literal or domain name).
    pub fn host(&self) -> String {
        match self {
            TargetAddr::Ip(ip) => ip.to_string(),
            TargetAddr::Domain(d) => d.clone(),
        }
    }
}

/// A parsed SOCKS5 CONNECT request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socks5Request {
    pub addr: TargetAddr,
    pub port: u16,
}

impl Socks5Request {
    pub fn host(&self) -> String {
        self.addr.host()
    }
}

// ---------------------------------------------------------------------------
// Pure parsing / encoding (no I/O — unit-tested)
// ---------------------------------------------------------------------------

/// Parse a client greeting (`VER NMETHODS METHODS...`) and pick an auth method.
/// Only [`METHOD_NO_AUTH`] is acceptable.
pub fn select_method(greeting: &[u8]) -> Result<u8, Socks5Error> {
    if greeting.len() < 2 {
        return Err(Socks5Error::Malformed("greeting too short"));
    }
    if greeting[0] != SOCKS5_VERSION {
        return Err(Socks5Error::BadVersion(greeting[0]));
    }
    let n = greeting[1] as usize;
    if greeting.len() != 2 + n {
        return Err(Socks5Error::Malformed("greeting method count mismatch"));
    }
    if greeting[2..].contains(&METHOD_NO_AUTH) {
        Ok(METHOD_NO_AUTH)
    } else {
        Err(Socks5Error::NoAcceptableMethods)
    }
}

/// Parse a CONNECT request body (`VER CMD RSV ATYP DST.ADDR DST.PORT`).
pub fn parse_request(buf: &[u8]) -> Result<Socks5Request, Socks5Error> {
    if buf.len() < 4 {
        return Err(Socks5Error::Malformed("request header too short"));
    }
    if buf[0] != SOCKS5_VERSION {
        return Err(Socks5Error::BadVersion(buf[0]));
    }
    if buf[1] != CMD_CONNECT {
        return Err(Socks5Error::UnsupportedCommand(buf[1]));
    }
    // buf[2] is RSV; per spec it must be 0x00 but we are lenient and ignore it.
    let atyp = buf[3];
    let rest = &buf[4..];
    let (addr, consumed) = match atyp {
        ATYP_IPV4 => {
            if rest.len() < 4 {
                return Err(Socks5Error::Malformed("ipv4 address truncated"));
            }
            let octets: [u8; 4] = [rest[0], rest[1], rest[2], rest[3]];
            (TargetAddr::Ip(IpAddr::V4(Ipv4Addr::from(octets))), 4)
        }
        ATYP_IPV6 => {
            if rest.len() < 16 {
                return Err(Socks5Error::Malformed("ipv6 address truncated"));
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&rest[..16]);
            (TargetAddr::Ip(IpAddr::V6(Ipv6Addr::from(octets))), 16)
        }
        ATYP_DOMAIN => {
            if rest.is_empty() {
                return Err(Socks5Error::Malformed("domain length missing"));
            }
            let len = rest[0] as usize;
            if len == 0 {
                return Err(Socks5Error::Malformed("empty domain name"));
            }
            if len > MAX_DOMAIN_LEN {
                return Err(Socks5Error::Malformed("domain name too long"));
            }
            if rest.len() < 1 + len {
                return Err(Socks5Error::Malformed("domain name truncated"));
            }
            let domain = std::str::from_utf8(&rest[1..1 + len])
                .map_err(|_| Socks5Error::Malformed("domain name is not valid UTF-8"))?
                .to_string();
            (TargetAddr::Domain(domain), 1 + len)
        }
        other => return Err(Socks5Error::UnsupportedAddressType(other)),
    };
    let after = &rest[consumed..];
    if after.len() < 2 {
        return Err(Socks5Error::Malformed("port truncated"));
    }
    let port = u16::from_be_bytes([after[0], after[1]]);
    Ok(Socks5Request { addr, port })
}

/// Encode a server reply (`VER REP RSV ATYP BND.ADDR BND.PORT`).
pub fn encode_reply(code: ReplyCode, bound: SocketAddr) -> Vec<u8> {
    let mut out = vec![SOCKS5_VERSION, code as u8, 0x00];
    match bound.ip() {
        IpAddr::V4(v4) => {
            out.push(ATYP_IPV4);
            out.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            out.push(ATYP_IPV6);
            out.extend_from_slice(&v6.octets());
        }
    }
    out.extend_from_slice(&bound.port().to_be_bytes());
    out
}

/// The unspecified bound address (`0.0.0.0:0`) used in replies where we have no
/// meaningful bound address to report (clients ignore it for CONNECT).
fn unspecified_bound() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
}

// ---------------------------------------------------------------------------
// Async helpers (bounded reads; drive the pure functions over a stream)
// ---------------------------------------------------------------------------

/// Read the greeting and reply with the chosen method. On success the method
/// reply (`05 00`) is written and `Ok(())` returned; on failure the
/// no-acceptable-methods reply (`05 FF`) is written (best effort) and the error
/// returned. A non-SOCKS5 first byte returns [`Socks5Error::BadVersion`] without
/// a reply.
pub async fn negotiate_method<S>(stream: &mut S) -> Result<(), Socks5Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] != SOCKS5_VERSION {
        return Err(Socks5Error::BadVersion(head[0]));
    }
    let nmethods = head[1] as usize;
    let mut methods = vec![0u8; nmethods];
    stream.read_exact(&mut methods).await?;

    let mut greeting = Vec::with_capacity(2 + nmethods);
    greeting.extend_from_slice(&head);
    greeting.extend_from_slice(&methods);

    match select_method(&greeting) {
        Ok(method) => {
            stream.write_all(&[SOCKS5_VERSION, method]).await?;
            Ok(())
        }
        Err(e) => {
            let _ = stream
                .write_all(&[SOCKS5_VERSION, METHOD_NO_ACCEPTABLE])
                .await;
            Err(e)
        }
    }
}

/// Read and parse a CONNECT request. Does **not** write a reply — the caller
/// sends the success/failure reply once it knows whether the forward opened.
/// Reads are bounded (a domain is at most [`MAX_DOMAIN_LEN`] bytes).
pub async fn read_request<S>(stream: &mut S) -> Result<Socks5Request, Socks5Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    // Validate version/command before reading any length-prefixed data so a
    // BIND / UDP ASSOCIATE request gets a clean reply instead of a stall.
    if header[0] != SOCKS5_VERSION {
        return Err(Socks5Error::BadVersion(header[0]));
    }
    if header[1] != CMD_CONNECT {
        return Err(Socks5Error::UnsupportedCommand(header[1]));
    }

    let mut buf = header.to_vec();
    match header[3] {
        ATYP_IPV4 => {
            let mut rest = [0u8; 6]; // 4 addr + 2 port
            stream.read_exact(&mut rest).await?;
            buf.extend_from_slice(&rest);
        }
        ATYP_IPV6 => {
            let mut rest = [0u8; 18]; // 16 addr + 2 port
            stream.read_exact(&mut rest).await?;
            buf.extend_from_slice(&rest);
        }
        ATYP_DOMAIN => {
            let mut len_byte = [0u8; 1];
            stream.read_exact(&mut len_byte).await?;
            let len = len_byte[0] as usize;
            if len == 0 {
                return Err(Socks5Error::Malformed("empty domain name"));
            }
            buf.push(len_byte[0]);
            let mut rest = vec![0u8; len + 2]; // domain + 2 port
            stream.read_exact(&mut rest).await?;
            buf.extend_from_slice(&rest);
        }
        other => return Err(Socks5Error::UnsupportedAddressType(other)),
    }
    parse_request(&buf)
}

/// Write a CONNECT reply with the given code and the unspecified bound address.
pub async fn write_reply<S>(stream: &mut S, code: ReplyCode) -> Result<(), Socks5Error>
where
    S: AsyncWrite + Unpin,
{
    stream
        .write_all(&encode_reply(code, unspecified_bound()))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_accepts_no_auth() {
        // VER=5, NMETHODS=1, METHOD=NO_AUTH.
        assert_eq!(select_method(&[0x05, 0x01, 0x00]).unwrap(), METHOD_NO_AUTH);
        // NO_AUTH present among several methods.
        assert_eq!(
            select_method(&[0x05, 0x02, 0x02, 0x00]).unwrap(),
            METHOD_NO_AUTH
        );
    }

    #[test]
    fn greeting_rejects_unsupported_methods() {
        // Only GSSAPI (0x01) and user/pass (0x02) offered.
        assert!(matches!(
            select_method(&[0x05, 0x02, 0x01, 0x02]),
            Err(Socks5Error::NoAcceptableMethods)
        ));
    }

    #[test]
    fn greeting_rejects_bad_version_and_short() {
        assert!(matches!(
            select_method(&[0x04, 0x01, 0x00]),
            Err(Socks5Error::BadVersion(0x04))
        ));
        assert!(matches!(
            select_method(&[0x05]),
            Err(Socks5Error::Malformed(_))
        ));
        // Method count mismatch.
        assert!(matches!(
            select_method(&[0x05, 0x02, 0x00]),
            Err(Socks5Error::Malformed(_))
        ));
    }

    #[test]
    fn request_ipv4_connect() {
        // VER CMD RSV ATYP=1 1.2.3.4 :443
        let buf = [0x05, 0x01, 0x00, 0x01, 1, 2, 3, 4, 0x01, 0xBB];
        let req = parse_request(&buf).unwrap();
        assert_eq!(req.addr, TargetAddr::Ip("1.2.3.4".parse().unwrap()));
        assert_eq!(req.port, 443);
        assert_eq!(req.host(), "1.2.3.4");
    }

    #[test]
    fn request_domain_connect() {
        // ATYP=3, len=11, "example.com", :80
        let mut buf = vec![0x05, 0x01, 0x00, 0x03, 11];
        buf.extend_from_slice(b"example.com");
        buf.extend_from_slice(&80u16.to_be_bytes());
        let req = parse_request(&buf).unwrap();
        assert_eq!(req.addr, TargetAddr::Domain("example.com".into()));
        assert_eq!(req.port, 80);
    }

    #[test]
    fn request_ipv6_connect() {
        let mut buf = vec![0x05, 0x01, 0x00, 0x04];
        let v6 = Ipv6Addr::LOCALHOST;
        buf.extend_from_slice(&v6.octets());
        buf.extend_from_slice(&8443u16.to_be_bytes());
        let req = parse_request(&buf).unwrap();
        assert_eq!(req.addr, TargetAddr::Ip(IpAddr::V6(v6)));
        assert_eq!(req.port, 8443);
    }

    #[test]
    fn request_rejects_bind_and_udp() {
        // CMD=2 (BIND).
        let bind = [0x05, 0x02, 0x00, 0x01, 1, 2, 3, 4, 0x00, 0x50];
        assert!(matches!(
            parse_request(&bind),
            Err(Socks5Error::UnsupportedCommand(0x02))
        ));
        // CMD=3 (UDP ASSOCIATE).
        let udp = [0x05, 0x03, 0x00, 0x01, 1, 2, 3, 4, 0x00, 0x50];
        assert!(matches!(
            parse_request(&udp),
            Err(Socks5Error::UnsupportedCommand(0x03))
        ));
    }

    #[test]
    fn request_rejects_short_and_bad_atyp() {
        // Truncated header.
        assert!(matches!(
            parse_request(&[0x05, 0x01]),
            Err(Socks5Error::Malformed(_))
        ));
        // Truncated IPv4 body.
        assert!(matches!(
            parse_request(&[0x05, 0x01, 0x00, 0x01, 1, 2]),
            Err(Socks5Error::Malformed(_))
        ));
        // Missing port after a complete IPv4 address.
        assert!(matches!(
            parse_request(&[0x05, 0x01, 0x00, 0x01, 1, 2, 3, 4]),
            Err(Socks5Error::Malformed(_))
        ));
        // Unknown address type 0x09.
        assert!(matches!(
            parse_request(&[0x05, 0x01, 0x00, 0x09, 0, 0]),
            Err(Socks5Error::UnsupportedAddressType(0x09))
        ));
        // Zero-length domain.
        assert!(matches!(
            parse_request(&[0x05, 0x01, 0x00, 0x03, 0, 0, 0]),
            Err(Socks5Error::Malformed(_))
        ));
    }

    #[test]
    fn reply_encoding() {
        let bound: SocketAddr = "0.0.0.0:0".parse().unwrap();
        assert_eq!(
            encode_reply(ReplyCode::Succeeded, bound),
            vec![0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
        );
        // A failure reply carries the failure code in byte 1.
        let r = encode_reply(ReplyCode::HostUnreachable, bound);
        assert_eq!(r[1], 0x04);
        assert_eq!(r[3], ATYP_IPV4);

        // IPv6 bound address uses ATYP=4 and 16 octets + 2 port.
        let v6: SocketAddr = "[::1]:1080".parse().unwrap();
        let r6 = encode_reply(ReplyCode::Succeeded, v6);
        assert_eq!(r6[3], ATYP_IPV6);
        assert_eq!(r6.len(), 3 + 1 + 16 + 2);
        assert_eq!(&r6[r6.len() - 2..], &1080u16.to_be_bytes());
    }

    #[test]
    fn error_reply_codes() {
        assert_eq!(
            Socks5Error::UnsupportedCommand(2).reply_code(),
            ReplyCode::CommandNotSupported
        );
        assert_eq!(
            Socks5Error::UnsupportedAddressType(9).reply_code(),
            ReplyCode::AddressTypeNotSupported
        );
        assert_eq!(
            Socks5Error::Malformed("x").reply_code(),
            ReplyCode::GeneralFailure
        );
    }

    /// Drive the async handshake over an in-memory duplex pipe (no network):
    /// the client writes a greeting + domain CONNECT request, the server runs
    /// [`negotiate_method`] + [`read_request`].
    #[tokio::test]
    async fn async_handshake_over_duplex() {
        let (mut client, mut server) = tokio::io::duplex(256);

        let client = async move {
            // Greeting: NO AUTH.
            client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
            let mut method = [0u8; 2];
            client.read_exact(&mut method).await.unwrap();
            assert_eq!(method, [0x05, 0x00]);
            // CONNECT example.com:443.
            let mut req = vec![0x05, 0x01, 0x00, 0x03, 11];
            req.extend_from_slice(b"example.com");
            req.extend_from_slice(&443u16.to_be_bytes());
            client.write_all(&req).await.unwrap();
        };

        let server = async move {
            negotiate_method(&mut server).await.unwrap();
            read_request(&mut server).await.unwrap()
        };

        let (_, req) = tokio::join!(client, server);
        assert_eq!(req.addr, TargetAddr::Domain("example.com".into()));
        assert_eq!(req.port, 443);
    }

    /// An unacceptable greeting yields the `0xFF` method reply and an error.
    #[tokio::test]
    async fn async_handshake_rejects_bad_methods() {
        let (mut client, mut server) = tokio::io::duplex(64);
        let client = async move {
            // Only username/password (0x02) offered — not acceptable.
            client.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
            let mut method = [0u8; 2];
            client.read_exact(&mut method).await.unwrap();
            method
        };
        let server = async move { negotiate_method(&mut server).await };
        let (method, res) = tokio::join!(client, server);
        assert_eq!(method, [0x05, 0xFF]);
        assert!(matches!(res, Err(Socks5Error::NoAcceptableMethods)));
    }
}
