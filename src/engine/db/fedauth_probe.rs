//! Ask an Azure SQL server where its Entra ID users sign in, as the
//! Microsoft drivers do for `ActiveDirectoryInteractive` (MS-TDS
//! FEDAUTHINFO): PRELOGIN, a TLS handshake inside PRELOGIN packets, then a
//! LOGIN7 asking for Entra ID through the ADAL "interactive" workflow. The
//! server answers with its STS URL (authority and tenant) and SPN, then waits
//! for a token that is never sent: the connection is closed at once. No user
//! name, password or token ever leaves.
//!
//! Packet layouts adapted from tiberius 0.12 (MIT / Apache-2.0:
//! `tds/codec/{header,pre_login,login}.rs`, `client/tls.rs`), checked
//! against mssql-jdbc (`SQLServerConnection.writeFedAuthFeatureRequest`,
//! `processFedAuthInfo`). Everything the server sends is read with bounds
//! checks: a malformed answer is an error, never a panic.

use std::io::{Read, Write};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, ConnectionCommon, RootCertStore};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Where a server's Entra ID users sign in (MS-TDS FEDAUTHINFO).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FedAuthInfo {
    /// Authority and tenant, e.g. `https://login.windows.net/<tenant-guid>`.
    pub sts_url: String,
    /// Resource the token is for, e.g. `https://database.windows.net/`.
    pub spn: String,
}

/// Opening the TCP connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The whole probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Ask the server where its Entra ID users sign in (MS-TDS FEDAUTHINFO).
/// Ok(None): the server doesn't do Entra ID federated auth (on-prem SQL Server…).
pub async fn probe(host: &str, port: u16) -> Result<Option<FedAuthInfo>> {
    probe_with(host, port, Trust::System, PROBE_TIMEOUT).await
}

/// How the server's certificate is checked.
#[derive(Debug, Clone, Copy)]
enum Trust {
    /// Against the system's roots, for the host name: always, outside tests.
    System,
    /// Whatever it is: the self-signed certificate of the test SQL Server
    /// container. Only in test builds.
    #[cfg(test)]
    AnyCertificate,
}

/// `probe`, with the certificate checked as `trust` says and given up after
/// `timeout`.
async fn probe_with(
    host: &str,
    port: u16,
    trust: Trust,
    timeout: Duration,
) -> Result<Option<FedAuthInfo>> {
    let run = async {
        let tcp = tokio::time::timeout(
            CONNECT_TIMEOUT.min(timeout),
            TcpStream::connect((host, port)),
        )
        .await
        .map_err(|_| anyhow!("Le serveur {host}:{port} ne répond pas"))?
        .with_context(|| format!("Connexion à {host}:{port} impossible"))?;
        tcp.set_nodelay(true)?;
        exchange(tcp, host, trust).await
    };
    tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| anyhow!("Le serveur {host}:{port} n'a pas répondu à temps"))?
}

/// The probe's conversation with the server on `stream`, whose name is
/// `host`.
async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    host: &str,
    trust: Trust,
) -> Result<Option<FedAuthInfo>> {
    // 1. PRELOGIN, in clear.
    let mut incoming = Packets::default();
    send(&mut stream, &framed(PRELOGIN, &prelogin_request())).await?;
    let answer = receive(&mut stream, &mut incoming, |p| p.message(REPLY)).await?;
    let prelogin = parse_prelogin_reply(&answer)?;
    if !matches!(prelogin.encryption, ENCRYPT_ON | ENCRYPT_REQ) {
        bail!("Le serveur ne propose pas de connexion chiffrée");
    }

    // 2. TLS, its handshake inside PRELOGIN packets.
    let name = ServerName::try_from(host.to_string())
        .map_err(|_| anyhow!("Nom de serveur invalide pour TLS : {host}"))?;
    let mut tls = ClientConnection::new(tls_config(trust)?, name)
        .map_err(|e| anyhow!("Échec de la négociation TLS : {e}"))?;
    handshake(&mut stream, &mut tls, &mut incoming).await?;

    // 3. LOGIN7 and its answer, inside TLS. Bytes already received after the
    // handshake are TLS records.
    let login = login7(&client_host_name(), host, prelogin.fed_auth_required);
    tls.writer().write_all(&framed(LOGIN7, &login))?;
    send(&mut stream, &drain_tls(&mut tls)?).await?;
    let mut plain = Packets::default();
    let mut received = incoming.take_rest();
    let mut chunk = vec![0u8; CHUNK];
    let answer = loop {
        plain.push(&feed(&mut tls, &received)?)?;
        if let Some(answer) = plain.message(REPLY)? {
            break answer;
        }
        if tls.wants_write() {
            send(&mut stream, &drain_tls(&mut tls)?).await?;
        }
        let n = stream
            .read(&mut chunk)
            .await
            .context("Lecture de la réponse du serveur impossible")?;
        if n == 0 {
            bail!("Le serveur a fermé la connexion");
        }
        received = chunk[..n].to_vec();
    };

    // The server now waits for a token: hang up instead.
    tls.send_close_notify();
    if let Ok(bytes) = drain_tls(&mut tls) {
        let _ = stream.write_all(&bytes).await;
    }
    let _ = stream.shutdown().await;
    parse_login_reply(&answer)
}

// ========== TLS ==========

/// rustls configuration for `trust`.
fn tls_config(trust: Trust) -> Result<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| anyhow!("Configuration TLS impossible : {e}"))?;
    let config = match trust {
        Trust::System => builder
            .with_root_certificates(system_roots()?)
            .with_no_client_auth(),
        #[cfg(test)]
        Trust::AnyCertificate => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(tests::AcceptAnyCertificate(provider)))
            .with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

/// The system's root certificates, as tiberius and sqlx use them (loaded
/// once).
fn system_roots() -> Result<Arc<RootCertStore>> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    if let Some(roots) = ROOTS.get() {
        return Ok(roots.clone());
    }
    let found = rustls_native_certs::load_native_certs();
    let mut roots = RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(found.certs);
    if added == 0 {
        let why: Vec<String> = found.errors.iter().map(ToString::to_string).collect();
        bail!(
            "Aucun certificat racine du système n'a pu être chargé ({})",
            why.join(" ; ")
        );
    }
    Ok(ROOTS.get_or_init(|| Arc::new(roots)).clone())
}

/// The TLS handshake of `tls` on `stream`, every flight inside PRELOGIN
/// packets (what SQL Server expects before TDS 8). Generic over the side for
/// the tests' fake server.
async fn handshake<S: AsyncRead + AsyncWrite + Unpin, D>(
    stream: &mut S,
    tls: &mut ConnectionCommon<D>,
    incoming: &mut Packets,
) -> Result<()> {
    while tls.is_handshaking() {
        if tls.wants_write() {
            send(stream, &framed(PRELOGIN, &drain_tls(tls)?)).await?;
        } else {
            let packet = receive(stream, incoming, |p| p.packet(PRELOGIN)).await?;
            feed(tls, &packet.payload)?;
        }
    }
    // TLS 1.3: the client's Finished, still part of the handshake.
    if tls.wants_write() {
        send(stream, &framed(PRELOGIN, &drain_tls(tls)?)).await?;
    }
    Ok(())
}

/// Give `tls` the bytes `received`; the plaintext they held.
fn feed<D>(tls: &mut ConnectionCommon<D>, mut received: &[u8]) -> Result<Vec<u8>> {
    let mut plain = Vec::new();
    while !received.is_empty() {
        let taken = tls
            .read_tls(&mut received)
            .context("Réception TLS impossible")?;
        // After the peer's close_notify rustls takes nothing more: looping
        // would spin forever, out of reach of the probe's timeout.
        if taken == 0 {
            bail!("Le serveur a fermé la connexion");
        }
        let state = tls.process_new_packets().map_err(|e| match e {
            rustls::Error::InvalidCertificate(_) => anyhow!("Certificat du serveur refusé : {e}"),
            _ => anyhow!("Échec de la négociation TLS : {e}"),
        })?;
        let available = state.plaintext_bytes_to_read();
        if available > 0 {
            let start = plain.len();
            plain.resize(start + available, 0);
            tls.reader()
                .read_exact(&mut plain[start..])
                .context("Réception TLS impossible")?;
        }
    }
    Ok(plain)
}

/// What `tls` has to send.
fn drain_tls<D>(tls: &mut ConnectionCommon<D>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while tls.wants_write() {
        tls.write_tls(&mut out).context("Envoi TLS impossible")?;
    }
    Ok(out)
}

// ========== Packets ==========

const PRELOGIN: u8 = 0x12;
const LOGIN7: u8 = 0x10;
/// Tabular result: every answer of the server.
const REPLY: u8 = 0x04;
/// Last packet of a message.
const STATUS_EOM: u8 = 0x01;
const HEADER_LEN: usize = 8;
/// Size of our packets, and the one asked in LOGIN7.
const PACKET_SIZE: usize = 4096;
/// Largest answer accepted from the server.
const MAX_MESSAGE: usize = 1 << 20;
/// Reads from the network.
const CHUNK: usize = 8192;

/// `payload` as a TDS message of type `kind`: packets of at most
/// `PACKET_SIZE` bytes (header: type, status, big-endian length, SPID,
/// packet number, window), the last one flagged end of message.
fn framed(kind: u8, payload: &[u8]) -> Vec<u8> {
    let chunks: Vec<&[u8]> = if payload.is_empty() {
        vec![&[]]
    } else {
        payload.chunks(PACKET_SIZE - HEADER_LEN).collect()
    };
    let mut out = Vec::with_capacity(payload.len() + HEADER_LEN * chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let status = if i + 1 == chunks.len() { STATUS_EOM } else { 0 };
        let [len_hi, len_lo] = ((chunk.len() + HEADER_LEN) as u16).to_be_bytes();
        // Packet numbers start at 1 and wrap (MS-TDS: ignored by servers).
        let number = (i + 1) as u8;
        out.extend_from_slice(&[kind, status, len_hi, len_lo, 0, 0, number, 0]);
        out.extend_from_slice(chunk);
    }
    out
}

/// One TDS packet.
struct Packet {
    /// End of message.
    last: bool,
    payload: Vec<u8>,
}

/// TDS packets and messages out of the bytes received.
#[derive(Default)]
struct Packets {
    /// Received, not yet split into packets.
    buf: Vec<u8>,
    /// Payload of the message being received.
    message: Vec<u8>,
}

impl Packets {
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        if self.buf.len() + self.message.len() + bytes.len() > MAX_MESSAGE {
            bail!("Réponse du serveur trop longue");
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    /// The next packet, of type `kind`, once it is all there (another type
    /// is an error as soon as it shows).
    fn packet(&mut self, kind: u8) -> Result<Option<Packet>> {
        if let Some(&first) = self.buf.first() {
            expect_kind(first, kind)?;
        }
        let Some(header) = self.buf.get(..HEADER_LEN) else {
            return Ok(None);
        };
        let status = header[1];
        let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
        if length < HEADER_LEN {
            bail!("Réponse du serveur illisible (paquet TDS de {length} octets)");
        }
        if self.buf.len() < length {
            return Ok(None);
        }
        let payload = self.buf[HEADER_LEN..length].to_vec();
        self.buf.drain(..length);
        Ok(Some(Packet {
            last: status & STATUS_EOM != 0,
            payload,
        }))
    }

    /// The payload of the next message, of type `kind`, once it is all there.
    fn message(&mut self, kind: u8) -> Result<Option<Vec<u8>>> {
        while let Some(packet) = self.packet(kind)? {
            self.message.extend_from_slice(&packet.payload);
            if packet.last {
                return Ok(Some(std::mem::take(&mut self.message)));
            }
        }
        Ok(None)
    }

    /// The bytes received after the last packet.
    fn take_rest(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

fn expect_kind(kind: u8, expected: u8) -> Result<()> {
    if kind != expected {
        bail!(
            "Réponse inattendue du serveur (paquet TDS de type 0x{kind:02X} au lieu de \
             0x{expected:02X}) : est-ce bien SQL Server ?"
        );
    }
    Ok(())
}

/// Read from `stream` into `packets` until `next` finds what it wants there.
async fn receive<S: AsyncRead + Unpin, T>(
    stream: &mut S,
    packets: &mut Packets,
    mut next: impl FnMut(&mut Packets) -> Result<Option<T>>,
) -> Result<T> {
    let mut chunk = vec![0u8; CHUNK];
    loop {
        if let Some(found) = next(packets)? {
            return Ok(found);
        }
        let n = stream
            .read(&mut chunk)
            .await
            .context("Lecture de la réponse du serveur impossible")?;
        if n == 0 {
            bail!("Le serveur a fermé la connexion");
        }
        packets.push(&chunk[..n])?;
    }
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, bytes: &[u8]) -> Result<()> {
    stream
        .write_all(bytes)
        .await
        .context("Envoi au serveur impossible")?;
    stream.flush().await.context("Envoi au serveur impossible")
}

/// Bounds-checked reading of what the server sent.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| anyhow!("Réponse du serveur tronquée"))?;
        let taken = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16_le(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u16_be(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32_le(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

// ========== PRELOGIN ==========

const PL_VERSION: u8 = 0x00;
const PL_ENCRYPTION: u8 = 0x01;
const PL_INSTOPT: u8 = 0x02;
const PL_THREADID: u8 = 0x03;
const PL_MARS: u8 = 0x04;
const PL_FEDAUTHREQUIRED: u8 = 0x06;
const PL_TERMINATOR: u8 = 0xFF;
#[cfg(test)]
const ENCRYPT_OFF: u8 = 0x00;
const ENCRYPT_ON: u8 = 0x01;
#[cfg(test)]
const ENCRYPT_NOT_SUP: u8 = 0x02;
const ENCRYPT_REQ: u8 = 0x03;

/// PRELOGIN asking for encryption and whether Entra ID is required.
fn prelogin_request() -> Vec<u8> {
    let options: [(u8, &[u8]); 6] = [
        // Client version: informational, none given.
        (PL_VERSION, &[0; 6]),
        (PL_ENCRYPTION, &[ENCRYPT_ON]),
        // Any instance.
        (PL_INSTOPT, &[0x00]),
        (PL_THREADID, &[0; 4]),
        // No MARS.
        (PL_MARS, &[0x00]),
        // Must be 1 when sent.
        (PL_FEDAUTHREQUIRED, &[0x01]),
    ];
    let mut table = Vec::new();
    let mut data = Vec::new();
    let start = options.len() * 5 + 1;
    for (token, value) in options {
        table.push(token);
        table.extend_from_slice(&((start + data.len()) as u16).to_be_bytes());
        table.extend_from_slice(&(value.len() as u16).to_be_bytes());
        data.extend_from_slice(value);
    }
    table.push(PL_TERMINATOR);
    table.extend(data);
    table
}

/// What matters in the server's PRELOGIN answer.
#[derive(Debug, PartialEq, Eq)]
struct PreloginReply {
    encryption: u8,
    /// To echo in LOGIN7's FEDAUTH feature.
    fed_auth_required: bool,
}

fn parse_prelogin_reply(payload: &[u8]) -> Result<PreloginReply> {
    let unreadable = || anyhow!("Réponse PRELOGIN du serveur illisible");
    let mut table = Reader::new(payload);
    let mut encryption = None;
    let mut fed_auth_required = false;
    loop {
        let token = table.u8()?;
        if token == PL_TERMINATOR {
            break;
        }
        let offset = usize::from(table.u16_be()?);
        let length = usize::from(table.u16_be()?);
        let value = payload
            .get(offset..offset + length)
            .ok_or_else(unreadable)?;
        match token {
            PL_ENCRYPTION => encryption = Some(*value.first().ok_or_else(unreadable)?),
            PL_FEDAUTHREQUIRED => {
                fed_auth_required = match value {
                    [0] => false,
                    [1] => true,
                    _ => return Err(unreadable()),
                };
            }
            _ => {}
        }
    }
    let encryption =
        encryption.ok_or_else(|| anyhow!("Le serveur n'a pas indiqué son chiffrement"))?;
    Ok(PreloginReply {
        encryption,
        fed_auth_required,
    })
}

// ========== LOGIN7 ==========

const TDS_7_4: u32 = 0x7400_0004;
/// Fixed part of LOGIN7, up to the variable data.
const LOGIN7_FIXED_LEN: usize = 94;
/// OptionFlags1: fUseDB, fDatabase fatal (as tiberius).
const OPTION_FLAGS_1: u8 = 0x20 | 0x40;
/// OptionFlags2: fLanguage fatal, fODBC.
const OPTION_FLAGS_2: u8 = 0x01 | 0x02;
/// OptionFlags3: fUnknownCollationHandling, fExtension (FeatureExt follows).
const OPTION_FLAGS_3: u8 = 0x08 | 0x10;
const APP_NAME: &str = "storingUnicorns";
/// Longest name of a LOGIN7 field, in UTF-16 units.
const MAX_NAME: usize = 128;
const FEATURE_FEDAUTH: u8 = 0x02;
/// FEDAUTH library "ADAL": the server gives FEDAUTHINFO before the token.
const FEDAUTH_LIBRARY_ADAL: u8 = 0x02;
const ADAL_WORKFLOW_INTERACTIVE: u8 = 0x03;
const FEATURE_TERMINATOR: u8 = 0xFF;

/// LOGIN7 (TDS 7.4) of the probe: no user name, no password, no database,
/// and the FEDAUTH feature for the ADAL "interactive" workflow, which makes
/// the server answer FEDAUTHINFO. `fed_auth_echo`: the server's
/// FEDAUTHREQUIRED, echoed as mssql-jdbc does.
fn login7(client_host: &str, server_name: &str, fed_auth_echo: bool) -> Vec<u8> {
    let utf16 = |s: &str| -> Vec<u8> {
        s.encode_utf16()
            .take(MAX_NAME)
            .flat_map(u16::to_le_bytes)
            .collect()
    };
    let (host, app, server) = (utf16(client_host), utf16(APP_NAME), utf16(server_name));
    let mut f = Fields::default();
    f.add(&host, host.len() / 2); // HostName
    f.add(&[], 0); // UserName
    f.add(&[], 0); // Password
    f.add(&app, app.len() / 2); // AppName
    f.add(&server, server.len() / 2); // ServerName
    let extension_at = f.data.len();
    f.add(&[0; 4], 4); // Extension: offset of FeatureExt, set below
    f.add(&[], 0); // CltIntName
    f.add(&[], 0); // Language
    f.add(&[], 0); // Database
    f.table.extend_from_slice(&[0; 6]); // ClientID
    f.add(&[], 0); // SSPI
    f.add(&[], 0); // AtchDBFile
    f.add(&[], 0); // ChangePassword
    f.table.extend_from_slice(&0u32.to_le_bytes()); // cbSSPILong

    let Fields { table, mut data } = f;
    let feature_ext_at = (LOGIN7_FIXED_LEN + data.len()) as u32;
    data[extension_at..extension_at + 4].copy_from_slice(&feature_ext_at.to_le_bytes());
    data.push(FEATURE_FEDAUTH);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.push((FEDAUTH_LIBRARY_ADAL << 1) | u8::from(fed_auth_echo));
    data.push(ADAL_WORKFLOW_INTERACTIVE);
    data.push(FEATURE_TERMINATOR);

    let total = LOGIN7_FIXED_LEN + data.len();
    let mut login = Vec::with_capacity(total);
    login.extend_from_slice(&(total as u32).to_le_bytes());
    login.extend_from_slice(&TDS_7_4.to_le_bytes());
    login.extend_from_slice(&(PACKET_SIZE as u32).to_le_bytes());
    login.extend_from_slice(&0u32.to_le_bytes()); // ClientProgVer
    login.extend_from_slice(&std::process::id().to_le_bytes());
    login.extend_from_slice(&0u32.to_le_bytes()); // ConnectionID
    login.extend_from_slice(&[OPTION_FLAGS_1, OPTION_FLAGS_2, 0, OPTION_FLAGS_3]);
    login.extend_from_slice(&0i32.to_le_bytes()); // ClientTimeZone
    login.extend_from_slice(&0u32.to_le_bytes()); // ClientLCID
    login.extend(table);
    login.extend(data);
    login
}

/// LOGIN7's OffsetLength table and the variable data it points into.
#[derive(Default)]
struct Fields {
    table: Vec<u8>,
    data: Vec<u8>,
}

impl Fields {
    /// A field holding `bytes`, of length `len` (in UTF-16 units or bytes,
    /// as the field counts it).
    fn add(&mut self, bytes: &[u8], len: usize) {
        let at = (LOGIN7_FIXED_LEN + self.data.len()) as u16;
        self.table.extend_from_slice(&at.to_le_bytes());
        self.table.extend_from_slice(&(len as u16).to_le_bytes());
        self.data.extend_from_slice(bytes);
    }
}

/// This machine's name, for the server's logs (like every driver).
fn client_host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

// ========== Answer to LOGIN7 ==========

const TOKEN_ERROR: u8 = 0xAA;
const TOKEN_INFO: u8 = 0xAB;
const TOKEN_LOGINACK: u8 = 0xAD;
const TOKEN_FEATUREEXTACK: u8 = 0xAE;
const TOKEN_ENVCHANGE: u8 = 0xE3;
const TOKEN_FEDAUTHINFO: u8 = 0xEE;
const TOKEN_DONE: u8 = 0xFD;
const INFO_STSURL: u8 = 0x01;
const INFO_SPN: u8 = 0x02;

/// The FEDAUTHINFO of the server's answer to LOGIN7 (its token stream);
/// `None` when an error, `DONE` or the end comes first.
fn parse_login_reply(stream: &[u8]) -> Result<Option<FedAuthInfo>> {
    let mut r = Reader::new(stream);
    while !r.is_empty() {
        match r.u8()? {
            TOKEN_FEDAUTHINFO => {
                let len = r.u32_le()? as usize;
                return parse_fedauthinfo(r.take(len)?).map(Some);
            }
            TOKEN_ERROR => {
                let len = usize::from(r.u16_le()?);
                let (number, message) = server_error(r.take(len)?);
                tracing::info!("FEDAUTHINFO probe: login refused, error {number} ({message})");
                return Ok(None);
            }
            TOKEN_INFO | TOKEN_LOGINACK | TOKEN_ENVCHANGE => {
                let len = usize::from(r.u16_le()?);
                r.take(len)?;
            }
            TOKEN_FEATUREEXTACK => loop {
                if r.u8()? == FEATURE_TERMINATOR {
                    break;
                }
                let len = r.u32_le()? as usize;
                r.take(len)?;
            },
            TOKEN_DONE => return Ok(None),
            other => bail!("Réponse du serveur illisible (jeton TDS 0x{other:02X} inattendu)"),
        }
    }
    Ok(None)
}

/// Number and message of an ERROR token's `body`, as far as they can be read.
fn server_error(body: &[u8]) -> (u32, String) {
    let mut r = Reader::new(body);
    let number = r.u32_le().unwrap_or_default();
    let message = r
        .take(2) // state, class
        .and_then(|_| r.u16_le())
        .and_then(|chars| r.take(usize::from(chars) * 2))
        .map(|text| {
            let units: Vec<u16> = text
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        })
        .unwrap_or_default();
    (number, message)
}

/// A FEDAUTHINFO token's data (after its length): `CountOfInfoIDs`, then as
/// many (`FedAuthInfoID`, `FedAuthInfoDataLen`, `FedAuthInfoDataOffset`),
/// offsets from the start of `data`, then the values in UTF-16LE.
fn parse_fedauthinfo(data: &[u8]) -> Result<FedAuthInfo> {
    let invalid = |why: &str| anyhow!("Réponse FEDAUTHINFO du serveur illisible ({why})");
    let mut r = Reader::new(data);
    let count = r.u32_le()? as usize;
    let table_end = count
        .checked_mul(9)
        .and_then(|n| n.checked_add(4))
        .filter(|&end| end <= data.len())
        .ok_or_else(|| invalid("trop d'entrées"))?;
    let (mut sts_url, mut spn) = (None, None);
    for _ in 0..count {
        let id = r.u8()?;
        let len = r.u32_le()? as usize;
        let offset = r.u32_le()? as usize;
        let value = offset
            .checked_add(len)
            .filter(|_| offset >= table_end)
            .and_then(|end| data.get(offset..end))
            .ok_or_else(|| invalid("position hors limites"))?;
        if value.len() % 2 != 0 {
            return Err(invalid("longueur impaire"));
        }
        let units: Vec<u16> = value
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let text = String::from_utf16(&units).map_err(|_| invalid("UTF-16 invalide"))?;
        match id {
            INFO_STSURL => sts_url = Some(text),
            INFO_SPN => spn = Some(text),
            _ => {}
        }
    }
    let non_empty = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    match (non_empty(sts_url), non_empty(spn)) {
        (Some(sts_url), Some(spn)) => Ok(FedAuthInfo { sts_url, spn }),
        _ => Err(invalid("STSURL et SPN attendus")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- Builders, after MS-TDS -----

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// A FEDAUTHINFO token (`0xEE`, length, then the data) with `entries`,
    /// their data in the same order after the table.
    fn fedauthinfo(entries: &[(u8, &str)]) -> Vec<u8> {
        let table_end = 4 + 9 * entries.len();
        let mut table = (entries.len() as u32).to_le_bytes().to_vec();
        let mut data = Vec::new();
        for (id, value) in entries {
            let bytes = utf16(value);
            table.push(*id);
            table.extend((bytes.len() as u32).to_le_bytes());
            table.extend(((table_end + data.len()) as u32).to_le_bytes());
            data.extend(bytes);
        }
        let mut body = table;
        body.extend(data);
        let mut token = vec![TOKEN_FEDAUTHINFO];
        token.extend((body.len() as u32).to_le_bytes());
        token.extend(body);
        token
    }

    /// A token with a 16-bit length (ENVCHANGE, INFO, LOGINACK, ERROR).
    fn token16(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut token = vec![kind];
        token.extend((body.len() as u16).to_le_bytes());
        token.extend(body);
        token
    }

    /// ERROR / INFO body: number, state, class, message, server, procedure,
    /// line.
    fn message_body(number: u32, text: &str) -> Vec<u8> {
        let mut body = number.to_le_bytes().to_vec();
        body.extend([1, 14]);
        body.extend((text.encode_utf16().count() as u16).to_le_bytes());
        body.extend(utf16(text));
        body.extend([0, 0]); // server name, procedure name: empty B_VARCHAR
        body.extend(1u32.to_le_bytes());
        body
    }

    fn env_change() -> Vec<u8> {
        // Packet size 4096 → 4096 (type 4, B_VARCHAR values).
        let mut body = vec![4, 4];
        body.extend(utf16("4096"));
        body.push(4);
        body.extend(utf16("4096"));
        token16(TOKEN_ENVCHANGE, &body)
    }

    fn feature_ext_ack() -> Vec<u8> {
        let mut token = vec![TOKEN_FEATUREEXTACK, 0x04];
        token.extend(1u32.to_le_bytes());
        token.push(1);
        token.push(0xFF);
        token
    }

    fn done() -> Vec<u8> {
        let mut token = vec![TOKEN_DONE];
        token.extend([0u8; 12]);
        token
    }

    const STS: &str = "https://login.windows.net/72f988bf-86f1-41af-91ab-2d7cd011db47";
    const SPN: &str = "https://database.windows.net/";

    fn expected() -> FedAuthInfo {
        FedAuthInfo {
            sts_url: STS.into(),
            spn: SPN.into(),
        }
    }

    /// Azure SQL's answer: other tokens, then FEDAUTHINFO.
    fn azure_reply() -> Vec<u8> {
        let mut reply = env_change();
        reply.extend(token16(TOKEN_INFO, &message_body(5701, "Changed database")));
        reply.extend(feature_ext_ack());
        reply.extend(fedauthinfo(&[(INFO_STSURL, STS), (INFO_SPN, SPN)]));
        reply
    }

    // ----- Token stream -----

    #[test]
    fn fedauthinfo_after_other_tokens() {
        assert_eq!(parse_login_reply(&azure_reply()).unwrap(), Some(expected()));
    }

    #[test]
    fn fedauthinfo_entries_in_any_order() {
        let reply = fedauthinfo(&[(INFO_SPN, SPN), (INFO_STSURL, STS)]);
        assert_eq!(parse_login_reply(&reply).unwrap(), Some(expected()));
        // Unknown entries are skipped.
        let reply = fedauthinfo(&[(0x09, "x"), (INFO_STSURL, STS), (INFO_SPN, SPN)]);
        assert_eq!(parse_login_reply(&reply).unwrap(), Some(expected()));
    }

    #[test]
    fn fedauthinfo_needs_both_entries() {
        for entries in [
            &[(INFO_STSURL, STS)][..],
            &[(INFO_SPN, SPN)],
            &[(INFO_STSURL, " "), (INFO_SPN, SPN)],
            &[],
        ] {
            assert!(
                parse_login_reply(&fedauthinfo(entries)).is_err(),
                "{entries:?}"
            );
        }
    }

    #[test]
    fn fedauthinfo_offset_out_of_bounds_is_an_error() {
        let token = fedauthinfo(&[(INFO_STSURL, STS), (INFO_SPN, SPN)]);
        // Body starts after the token type and its length.
        let body = 5;
        // Second entry's offset: past the end.
        let offset_at = body + 4 + 9 + 5;
        for bad in [u32::MAX, 10_000, (token.len() - body) as u32] {
            let mut t = token.clone();
            t[offset_at..offset_at + 4].copy_from_slice(&bad.to_le_bytes());
            assert!(parse_login_reply(&t).is_err(), "offset {bad}");
        }
        // Inside the entries table.
        let mut t = token.clone();
        t[offset_at..offset_at + 4].copy_from_slice(&2u32.to_le_bytes());
        assert!(parse_login_reply(&t).is_err());
        // Length past the end.
        let mut t = token.clone();
        t[offset_at - 4..offset_at].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_login_reply(&t).is_err());
        // More entries than the token holds.
        let mut t = token;
        t[body..body + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_login_reply(&t).is_err());
    }

    #[test]
    fn odd_length_or_invalid_utf16_is_an_error() {
        let mut t = fedauthinfo(&[(INFO_STSURL, STS), (INFO_SPN, SPN)]);
        // First entry's length, made odd.
        let len_at = 5 + 4 + 1;
        let len = u32::from_le_bytes(t[len_at..len_at + 4].try_into().unwrap());
        t[len_at..len_at + 4].copy_from_slice(&(len - 1).to_le_bytes());
        assert!(parse_login_reply(&t).is_err());
        // A lone surrogate in the STS URL.
        let spn = utf16(SPN);
        let mut body = 2u32.to_le_bytes().to_vec();
        body.push(INFO_STSURL);
        body.extend(2u32.to_le_bytes());
        body.extend(22u32.to_le_bytes());
        body.push(INFO_SPN);
        body.extend((spn.len() as u32).to_le_bytes());
        body.extend(24u32.to_le_bytes());
        body.extend(0xD800u16.to_le_bytes());
        body.extend(&spn);
        assert!(parse_fedauthinfo(&body).is_err());
        // The same with a valid STS URL is fine.
        body[22..24].copy_from_slice(&u16::from(b'x').to_le_bytes());
        assert_eq!(parse_fedauthinfo(&body).unwrap().sts_url, "x");
    }

    #[test]
    fn answer_without_fedauthinfo_is_none() {
        // On-premises SQL Server: the login is refused.
        let mut reply = env_change();
        reply.extend(token16(TOKEN_ERROR, &message_body(18456, "Login failed")));
        reply.extend(done());
        assert_eq!(parse_login_reply(&reply).unwrap(), None);
        // FEDAUTHINFO after an error doesn't count.
        let mut late = token16(TOKEN_ERROR, &message_body(18456, "Login failed"));
        late.extend(fedauthinfo(&[(INFO_STSURL, STS), (INFO_SPN, SPN)]));
        assert_eq!(parse_login_reply(&late).unwrap(), None);
        // A login without Entra ID.
        let mut reply = token16(TOKEN_LOGINACK, &[0; 10]);
        reply.extend(done());
        assert_eq!(parse_login_reply(&reply).unwrap(), None);
        assert_eq!(parse_login_reply(&env_change()).unwrap(), None);
        assert_eq!(parse_login_reply(&[]).unwrap(), None);
    }

    #[test]
    fn unknown_token_is_an_error() {
        let mut reply = env_change();
        reply.push(0x81); // COLMETADATA: not an answer to LOGIN7
        reply.extend([0; 16]);
        assert!(parse_login_reply(&reply).is_err());
    }

    /// Deterministic pseudo-random bytes (xorshift).
    fn garbage(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn truncated_or_garbage_answers_never_panic() {
        let reply = azure_reply();
        for end in 0..reply.len() {
            let _ = parse_login_reply(&reply[..end]);
        }
        let prelogin = prelogin_reply(ENCRYPT_ON, Some(1));
        for end in 0..prelogin.len() {
            let _ = parse_prelogin_reply(&prelogin[..end]);
        }
        let token = fedauthinfo(&[(INFO_STSURL, STS), (INFO_SPN, SPN)]);
        for end in 0..token.len() {
            let _ = parse_fedauthinfo(&token[5.min(end)..end]);
        }
        for seed in 0..2000u64 {
            let len = (seed % 300) as usize;
            let bytes = garbage(seed, len);
            let _ = parse_login_reply(&bytes);
            let _ = parse_prelogin_reply(&bytes);
            let _ = parse_fedauthinfo(&bytes);
            let mut packets = Packets::default();
            if packets.push(&bytes).is_ok() {
                while let Ok(Some(_)) = packets.message(REPLY) {}
            }
            // Valid answers with one byte changed.
            for valid in [&reply, &prelogin] {
                let mut v = valid.to_vec();
                let i = (seed as usize * 7919) % v.len();
                v[i] = garbage(seed, 1)[0];
                let _ = parse_login_reply(&v);
                let _ = parse_prelogin_reply(&v);
            }
        }
    }

    // ----- Packets -----

    #[test]
    fn packet_header_after_ms_tds() {
        let bytes = framed(PRELOGIN, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(
            bytes,
            [0x12, 0x01, 0x00, 0x0B, 0x00, 0x00, 0x01, 0x00, 0xAA, 0xBB, 0xCC]
        );
        assert_eq!(framed(LOGIN7, &[]), [0x10, 0x01, 0x00, 0x08, 0, 0, 1, 0]);
    }

    #[test]
    fn long_messages_are_split_into_packets() {
        let payload: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let bytes = framed(LOGIN7, &payload);
        assert_eq!(bytes.len(), 5000 + 2 * HEADER_LEN);
        // First packet: full, not the last.
        assert_eq!(&bytes[..8], [0x10, 0x00, 0x10, 0x00, 0, 0, 1, 0]);
        let second = &bytes[PACKET_SIZE..];
        let len = (5000 - (PACKET_SIZE - HEADER_LEN) + HEADER_LEN) as u16;
        assert_eq!(
            &second[..8],
            [0x10, 0x01, (len >> 8) as u8, len as u8, 0, 0, 2, 0]
        );

        // Read back, whatever the chunks they come in.
        let mut packets = Packets::default();
        for byte in &bytes[..bytes.len() - 1] {
            packets.push(&[*byte]).unwrap();
            assert_eq!(packets.message(LOGIN7).unwrap(), None);
        }
        packets.push(&bytes[bytes.len() - 1..]).unwrap();
        assert_eq!(packets.message(LOGIN7).unwrap(), Some(payload));
    }

    #[test]
    fn bad_packets_are_errors() {
        // Length shorter than the header.
        let mut packets = Packets::default();
        packets.push(&[0x04, 0x01, 0x00, 0x03, 0, 0, 1, 0]).unwrap();
        assert!(packets.message(REPLY).is_err());
        // Another type than expected (an HTTP server, say).
        let mut packets = Packets::default();
        packets.push(b"HTTP/1.1 400 Bad Request\r\n\r\n").unwrap();
        assert!(packets.message(REPLY).is_err());
        // Too long an answer.
        let mut packets = Packets::default();
        let chunk = framed(REPLY, &vec![0; 4000]);
        let chunk_without_eom: Vec<u8> = {
            let mut c = chunk.clone();
            c[1] = 0;
            c
        };
        let mut failed = false;
        for _ in 0..(MAX_MESSAGE / 4000 + 2) {
            if packets.push(&chunk_without_eom).is_err() {
                failed = true;
                break;
            }
            assert_eq!(packets.message(REPLY).unwrap(), None);
        }
        assert!(failed, "an endless answer must be refused");
    }

    // ----- PRELOGIN -----

    /// The options of a PRELOGIN payload: token → data.
    fn prelogin_options(payload: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut options = Vec::new();
        let mut i = 0;
        while payload[i] != PL_TERMINATOR {
            let offset = u16::from_be_bytes([payload[i + 1], payload[i + 2]]) as usize;
            let len = u16::from_be_bytes([payload[i + 3], payload[i + 4]]) as usize;
            options.push((payload[i], payload[offset..offset + len].to_vec()));
            i += 5;
        }
        options
    }

    #[test]
    fn prelogin_request_asks_for_encryption_and_fedauth() {
        let options = prelogin_options(&prelogin_request());
        let tokens: Vec<u8> = options.iter().map(|(t, _)| *t).collect();
        assert_eq!(
            tokens,
            [
                PL_VERSION,
                PL_ENCRYPTION,
                PL_INSTOPT,
                PL_THREADID,
                PL_MARS,
                PL_FEDAUTHREQUIRED
            ]
        );
        let value = |t: u8| options.iter().find(|(o, _)| *o == t).unwrap().1.clone();
        assert_eq!(value(PL_VERSION).len(), 6);
        assert_eq!(value(PL_ENCRYPTION), [ENCRYPT_ON]);
        assert_eq!(value(PL_INSTOPT), [0x00]);
        assert_eq!(value(PL_THREADID).len(), 4);
        assert_eq!(value(PL_MARS), [0x00]);
        assert_eq!(value(PL_FEDAUTHREQUIRED), [0x01]);
        // The data follows the table without gaps.
        let request = prelogin_request();
        assert_eq!(request.len(), 6 * 5 + 1 + 6 + 1 + 1 + 4 + 1 + 1);
    }

    /// A server's PRELOGIN answer: version, encryption, and FEDAUTHREQUIRED
    /// when given.
    fn prelogin_reply(encryption: u8, fed_auth: Option<u8>) -> Vec<u8> {
        let mut options = vec![
            (PL_VERSION, vec![16, 0, 0x10, 0x00, 0, 0]),
            (PL_ENCRYPTION, vec![encryption]),
            (PL_INSTOPT, vec![0]),
            (PL_THREADID, vec![]),
            (PL_MARS, vec![0]),
        ];
        if let Some(f) = fed_auth {
            options.push((PL_FEDAUTHREQUIRED, vec![f]));
        }
        let mut table = Vec::new();
        let mut data: Vec<u8> = Vec::new();
        let start = options.len() * 5 + 1;
        for (token, value) in &options {
            table.push(*token);
            table.extend(((start + data.len()) as u16).to_be_bytes());
            table.extend((value.len() as u16).to_be_bytes());
            data.extend(value);
        }
        table.push(PL_TERMINATOR);
        table.extend(data);
        table
    }

    #[test]
    fn prelogin_reply_gives_encryption_and_fedauth() {
        assert_eq!(
            parse_prelogin_reply(&prelogin_reply(ENCRYPT_ON, Some(1))).unwrap(),
            PreloginReply {
                encryption: ENCRYPT_ON,
                fed_auth_required: true
            }
        );
        assert_eq!(
            parse_prelogin_reply(&prelogin_reply(ENCRYPT_REQ, Some(0))).unwrap(),
            PreloginReply {
                encryption: ENCRYPT_REQ,
                fed_auth_required: false
            }
        );
        // Older servers don't answer FEDAUTHREQUIRED.
        assert!(
            !parse_prelogin_reply(&prelogin_reply(ENCRYPT_OFF, None))
                .unwrap()
                .fed_auth_required
        );
    }

    #[test]
    fn bad_prelogin_replies_are_errors() {
        // Only 0 and 1 are valid for FEDAUTHREQUIRED.
        assert!(parse_prelogin_reply(&prelogin_reply(ENCRYPT_ON, Some(2))).is_err());
        // No encryption option.
        let mut reply = vec![PL_VERSION, 0, 6, 0, 6, PL_TERMINATOR];
        reply.extend([16, 0, 0, 0, 0, 0]);
        assert!(parse_prelogin_reply(&reply).is_err());
        // Option data out of bounds.
        assert!(parse_prelogin_reply(&[PL_ENCRYPTION, 0, 200, 0, 1, PL_TERMINATOR]).is_err());
        assert!(
            parse_prelogin_reply(&[PL_ENCRYPTION, 0xFF, 0xFF, 0xFF, 0xFF, PL_TERMINATOR]).is_err()
        );
        // Empty encryption value; no terminator.
        assert!(parse_prelogin_reply(&[PL_ENCRYPTION, 0, 6, 0, 0, PL_TERMINATOR]).is_err());
        assert!(parse_prelogin_reply(&[PL_ENCRYPTION, 0, 5, 0, 1, ENCRYPT_ON]).is_err());
    }

    // ----- LOGIN7 -----

    /// Offset and length of the `index`-th OffsetLength field of a LOGIN7
    /// (0: HostName … 8: Database, then 9: SSPI, 10: AtchDBFile, 11:
    /// ChangePassword, after the 6-byte ClientID).
    fn field(login: &[u8], index: usize) -> (usize, usize) {
        let at = 36 + 4 * index + if index >= 9 { 6 } else { 0 };
        let u16_at = |i: usize| u16::from_le_bytes([login[i], login[i + 1]]) as usize;
        (u16_at(at), u16_at(at + 2))
    }

    fn string(login: &[u8], index: usize) -> String {
        let (offset, chars) = field(login, index);
        let units: Vec<u16> = login[offset..offset + 2 * chars]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    /// The FeatureExt block of a LOGIN7.
    fn feature_ext(login: &[u8]) -> &[u8] {
        let (offset, len) = field(login, 5);
        assert_eq!(len, 4, "cbExtension");
        let at = u32::from_le_bytes(login[offset..offset + 4].try_into().unwrap()) as usize;
        &login[at..]
    }

    #[test]
    fn login7_asks_for_entra_id_without_credentials() {
        let login = login7("PC-JANE", "srv.database.windows.net", false);
        assert_eq!(
            u32::from_le_bytes(login[0..4].try_into().unwrap()) as usize,
            login.len()
        );
        assert_eq!(&login[4..8], 0x7400_0004u32.to_le_bytes(), "TDS 7.4");
        assert_eq!(&login[8..12], 4096u32.to_le_bytes(), "packet size");
        assert_eq!(login[27] & 0x10, 0x10, "OptionFlags3: fExtension");
        assert_eq!(login[25] & 0x80, 0, "OptionFlags2: no integrated security");
        assert_eq!(string(&login, 0), "PC-JANE");
        assert_eq!(field(&login, 1).1, 0, "no user name");
        assert_eq!(field(&login, 2).1, 0, "no password");
        assert_eq!(string(&login, 3), "storingUnicorns");
        assert_eq!(string(&login, 4), "srv.database.windows.net");
        assert_eq!(field(&login, 8).1, 0, "no database");
        assert_eq!(field(&login, 9).1, 0, "no SSPI");
        assert_eq!(field(&login, 11).1, 0, "no password change");
        // Every variable field after the 94-byte fixed part.
        for i in 0..12 {
            assert!(field(&login, i).0 >= 94, "field {i}");
        }
        // FEDAUTH, 2 bytes of data: ADAL library without echo, workflow
        // "interactive"; then the terminator, which ends the message.
        assert_eq!(
            feature_ext(&login),
            [0x02, 2, 0, 0, 0, 0x02 << 1, 0x03, 0xFF]
        );
    }

    #[test]
    fn login7_echoes_fedauthrequired() {
        let login = login7("", "srv", true);
        assert_eq!(
            feature_ext(&login),
            [0x02, 2, 0, 0, 0, (0x02 << 1) | 1, 0x03, 0xFF]
        );
        assert_eq!(string(&login, 0), "");
    }

    #[test]
    fn login7_names_are_capped() {
        let long = "é".repeat(300);
        let login = login7(&long, &long, false);
        assert_eq!(string(&login, 0).chars().count(), 128);
        assert_eq!(string(&login, 4).chars().count(), 128);
        assert_eq!(feature_ext(&login).len(), 8);
    }

    // ----- Transport, against a fake SQL Server -----

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    /// Takes any server certificate: the test SQL Server container's is
    /// self-signed, and X.509 v1, which webpki can't even parse to check the
    /// handshake signatures.
    #[derive(Debug)]
    pub(super) struct AcceptAnyCertificate(pub(super) Arc<rustls::crypto::CryptoProvider>);

    impl ServerCertVerifier for AcceptAnyCertificate {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    /// Self-signed certificate for `localhost` (P-256), and its key.
    const CERT: &[u8] = include_bytes!("testdata/probe-localhost.cert.der");
    const KEY: &[u8] = include_bytes!("testdata/probe-localhost.key.der");

    fn server_tls(tls12_only: bool) -> rustls::ServerConnection {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ServerConfig::builder_with_provider(provider);
        let builder = if tls12_only {
            builder.with_protocol_versions(&[&rustls::version::TLS12])
        } else {
            builder.with_safe_default_protocol_versions()
        };
        let mut config = builder
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(CERT.to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec())),
            )
            .unwrap();
        // Tickets would go out after the handshake, outside PRELOGIN
        // packets: keep the handshake's end unambiguous.
        config.send_tls13_tickets = 0;
        rustls::ServerConnection::new(Arc::new(config)).unwrap()
    }

    /// What the fake server received.
    #[derive(Debug, Default)]
    struct Received {
        prelogin: Vec<u8>,
        login: Vec<u8>,
        /// Plaintext after LOGIN7 (a token would be there).
        after_login: Vec<u8>,
    }

    /// A SQL Server answering PRELOGIN with `prelogin`, then, over TLS 1.2
    /// (as SQL Server does) or 1.3, LOGIN7 with `reply`.
    async fn fake_server<S: AsyncRead + AsyncWrite + Unpin>(
        mut stream: S,
        prelogin: Vec<u8>,
        reply: Vec<u8>,
        tls12_only: bool,
    ) -> Result<Received> {
        let mut received = Received::default();
        let mut incoming = Packets::default();
        received.prelogin = receive(&mut stream, &mut incoming, |p| p.message(PRELOGIN)).await?;
        send(&mut stream, &framed(REPLY, &prelogin)).await?;
        let mut tls = server_tls(tls12_only);
        handshake(&mut stream, &mut tls, &mut incoming).await?;
        let mut plain = Packets::default();
        let mut raw = incoming.take_rest();
        let mut chunk = vec![0; 8192];
        loop {
            plain.push(&feed(&mut tls, &raw)?)?;
            if let Some(login) = plain.message(LOGIN7)? {
                received.login = login;
                break;
            }
            let n = stream.read(&mut chunk).await?;
            anyhow::ensure!(n > 0, "client gone before LOGIN7");
            raw = chunk[..n].to_vec();
        }
        std::io::Write::write_all(&mut tls.writer(), &framed(REPLY, &reply))?;
        send(&mut stream, &drain_tls(&mut tls)?).await?;
        // Whatever comes next, until the client is gone.
        while let Ok(n) = stream.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            match feed(&mut tls, &chunk[..n]) {
                Ok(p) => received.after_login.extend(p),
                Err(_) => break,
            }
        }
        Ok(received)
    }

    async fn run_against(
        prelogin: Vec<u8>,
        reply: Vec<u8>,
        tls12_only: bool,
        trust: Trust,
    ) -> (Result<Option<FedAuthInfo>>, Result<Received>) {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(fake_server(server, prelogin, reply, tls12_only));
        let probed = exchange(client, "localhost", trust).await;
        (probed, server.await.unwrap())
    }

    #[tokio::test]
    async fn probe_reads_fedauthinfo_over_tls_and_sends_no_token() {
        for tls12_only in [true, false] {
            let (probed, received) = run_against(
                prelogin_reply(ENCRYPT_ON, Some(1)),
                azure_reply(),
                tls12_only,
                Trust::AnyCertificate,
            )
            .await;
            assert_eq!(
                probed.unwrap(),
                Some(expected()),
                "TLS 1.2 only: {tls12_only}"
            );
            let received = received.unwrap();
            assert_eq!(received.prelogin, prelogin_request());
            // FEDAUTHREQUIRED answered 1: echoed.
            assert_eq!(
                received.login,
                login7(&client_host_name(), "localhost", true)
            );
            assert!(
                received.after_login.is_empty(),
                "{:?}",
                received.after_login
            );
        }
    }

    #[tokio::test]
    async fn on_premises_server_gives_none() {
        let mut reply = token16(
            TOKEN_ERROR,
            &message_body(18456, "Login failed for user ''."),
        );
        reply.extend(done());
        let (probed, received) = run_against(
            prelogin_reply(ENCRYPT_REQ, None),
            reply,
            true,
            Trust::AnyCertificate,
        )
        .await;
        assert_eq!(probed.unwrap(), None);
        // No FEDAUTHREQUIRED in the answer: no echo.
        assert_eq!(feature_ext(&received.unwrap().login)[5], 0x02 << 1);
    }

    #[tokio::test]
    async fn untrusted_certificate_is_refused_before_login7() {
        let (probed, received) = run_against(
            prelogin_reply(ENCRYPT_ON, Some(1)),
            azure_reply(),
            true,
            Trust::System,
        )
        .await;
        let e = probed.unwrap_err().to_string();
        assert!(e.contains("Certificat du serveur refusé"), "{e}");
        assert!(received.is_err(), "{received:?}");
    }

    /// A server that closes TLS (close_notify) instead of answering LOGIN7,
    /// then sends more bytes and stays connected.
    async fn closing_server<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S) -> Result<()> {
        let mut incoming = Packets::default();
        receive(&mut stream, &mut incoming, |p| p.message(PRELOGIN)).await?;
        let answer = prelogin_reply(ENCRYPT_ON, Some(1));
        send(&mut stream, &framed(REPLY, &answer)).await?;
        let mut tls = server_tls(true);
        handshake(&mut stream, &mut tls, &mut incoming).await?;
        let mut plain = Packets::default();
        let mut raw = incoming.take_rest();
        let mut chunk = vec![0; 8192];
        loop {
            plain.push(&feed(&mut tls, &raw)?)?;
            if plain.message(LOGIN7)?.is_some() {
                break;
            }
            let n = stream.read(&mut chunk).await?;
            anyhow::ensure!(n > 0, "client gone before LOGIN7");
            raw = chunk[..n].to_vec();
        }
        tls.send_close_notify();
        send(&mut stream, &drain_tls(&mut tls)?).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        // An application data record after the close.
        send(&mut stream, &[0x17, 0x03, 0x03, 0x00, 0x01, 0x00]).await?;
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    }

    #[test]
    fn bytes_after_close_notify_end_the_probe() {
        // On its own runtime and thread: a probe spinning in `feed` would
        // keep its timeout from ever firing.
        let (done, outcome) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let probed = runtime.block_on(async {
                let (client, server) = tokio::io::duplex(64 * 1024);
                let server = tokio::spawn(closing_server(server));
                let probed = tokio::time::timeout(
                    Duration::from_secs(5),
                    exchange(client, "localhost", Trust::AnyCertificate),
                )
                .await;
                server.abort();
                probed.map(|r| r.map_err(|e| e.to_string()))
            });
            let _ = done.send(probed);
        });
        let probed = outcome
            .recv_timeout(Duration::from_secs(10))
            .expect("the probe spins after close_notify");
        let e = probed.expect("no timeout").unwrap_err();
        assert!(e.contains("fermé"), "{e}");
    }

    /// The probe against a server that reads PRELOGIN then sends `answer`
    /// and closes.
    async fn probe_answered(answer: Vec<u8>) -> Result<Option<FedAuthInfo>> {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let mut incoming = Packets::default();
            let _ = receive(&mut server, &mut incoming, |p| p.message(PRELOGIN)).await;
            let _ = server.write_all(&answer).await;
            let _ = server.shutdown().await;
        });
        exchange(client, "localhost", Trust::AnyCertificate).await
    }

    #[tokio::test]
    async fn servers_that_are_not_azure_sql_are_errors() {
        let refused = probe_answered(framed(REPLY, &prelogin_reply(ENCRYPT_NOT_SUP, None)))
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("chiffr"), "{refused}");
        // An HTTP server.
        assert!(probe_answered(b"HTTP/1.1 400 Bad Request\r\n\r\n".to_vec())
            .await
            .is_err());
        // Nothing at all.
        let closed = probe_answered(Vec::new()).await.unwrap_err().to_string();
        assert!(closed.contains("fermé"), "{closed}");
        // Garbage instead of the TLS handshake.
        let mut answer = framed(REPLY, &prelogin_reply(ENCRYPT_ON, Some(1)));
        answer.extend(framed(PRELOGIN, &garbage(7, 200)));
        assert!(probe_answered(answer).await.is_err());
    }

    #[tokio::test]
    async fn silent_server_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let started = std::time::Instant::now();
        let e = probe_with("127.0.0.1", port, Trust::System, Duration::from_millis(300))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("à temps"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
        server.abort();
    }

    #[tokio::test]
    async fn closed_port_is_an_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(probe_with("127.0.0.1", port, Trust::System, PROBE_TIMEOUT)
            .await
            .is_err());
    }

    /// Against the local SQL Server container (`SU_IT_MSSQL`, see
    /// `ops::integration_tests`): TLS and LOGIN7 go through, and it has no
    /// Entra ID.
    #[tokio::test]
    #[ignore]
    async fn integration_mssql_fedauth_probe() {
        let Ok(spec) = std::env::var("SU_IT_MSSQL") else {
            eprintln!("SU_IT_MSSQL not set, skipping");
            return;
        };
        let value = |key: &str| {
            spec.split(';')
                .filter_map(|p| p.split_once('='))
                .find(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim().to_string())
        };
        let host = value("host").unwrap_or_else(|| "127.0.0.1".into());
        let port = value("port").map_or(1433, |p| p.parse().unwrap());
        // Its certificate is self-signed: refused, as it would be outside
        // tests…
        let e = probe_with(&host, port, Trust::System, PROBE_TIMEOUT)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("Certificat du serveur refusé"), "{e}");
        // …and once accepted, the login gets an answer without FEDAUTHINFO.
        let probed = probe_with(&host, port, Trust::AnyCertificate, PROBE_TIMEOUT).await;
        assert_eq!(probed.unwrap(), None);
    }
}
