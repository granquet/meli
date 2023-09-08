/*
 * meli - imap module.
 *
 * Copyright 2017 - 2019 Manos Pitsidianakis
 *
 * This file is part of meli.
 *
 * meli is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * meli is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with meli. If not, see <http://www.gnu.org/licenses/>.
 */

use super::protocol_parser::{ImapLineSplit, ImapResponse, RequiredResponses, SelectResponse};
use crate::{
    backends::{BackendEvent, MailboxHash, RefreshEvent},
    email::parser::BytesExt,
    error::*,
    utils::{
        connections::{std_net::connect as tcp_stream_connect, Connection},
        futures::timeout,
    },
    LogLevel,
};
extern crate native_tls;
#[cfg(debug_assertions)]
use std::borrow::Cow;
use std::{
    collections::HashSet,
    convert::TryFrom,
    future::Future,
    iter::FromIterator,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use futures::io::{AsyncReadExt, AsyncWriteExt};
use imap_codec::{
    encode::{Encoder, Fragment},
    imap_types::{
        auth::AuthMechanism,
        command::{Command, CommandBody},
        core::{AString, LiteralMode, NonEmptyVec, Tag},
        extensions::{compress::CompressionAlgorithm, enable::CapabilityEnable},
        mailbox::Mailbox,
        search::SearchKey,
        secret::Secret,
        sequence::SequenceSet,
        status::StatusDataItemName,
    },
    CommandCodec,
};
use native_tls::TlsConnector;
pub use smol::Async as AsyncWrapper;

const IMAP_PROTOCOL_TIMEOUT: Duration = Duration::from_secs(60 * 28);

macro_rules! imap_trace {
    ($conn:expr, $fmt:literal, $($t:tt)*) => {
        #[cfg(debug_assertions)]
        log::trace!(std::concat!("{} ", $fmt), $conn.id, $($t)*);
    };
    ($conn:expr, $fmt:literal) => {
        #[cfg(debug_assertions)]
        log::trace!(std::concat!("{} ", $fmt), $conn.id);
    };
}

use super::{protocol_parser, Capabilities, ImapServerConf, UIDStore};

#[derive(Debug, Clone, Copy)]
pub enum SyncPolicy {
    None,
    /// RFC4549 `Synch Ops for Disconnected IMAP4 Clients` <https://tools.ietf.org/html/rfc4549>
    Basic,
    /// RFC7162 `IMAP Extensions: Quick Flag Changes Resynchronization
    /// (CONDSTORE) and Quick Mailbox Resynchronization (QRESYNC)`
    Condstore,
    CondstoreQresync,
}

#[derive(Debug, Clone, Copy)]
pub enum ImapProtocol {
    IMAP { extension_use: ImapExtensionUse },
    ManageSieve,
}

#[derive(Debug, Clone, Copy)]
pub struct ImapExtensionUse {
    pub condstore: bool,
    pub idle: bool,
    pub deflate: bool,
    pub oauth2: bool,
}

impl Default for ImapExtensionUse {
    fn default() -> Self {
        Self {
            condstore: true,
            idle: true,
            deflate: true,
            oauth2: false,
        }
    }
}

#[derive(Debug)]
pub struct ImapStream {
    pub cmd_id: usize,
    #[cfg(debug_assertions)]
    pub id: Cow<'static, str>,
    pub stream: AsyncWrapper<Connection>,
    pub protocol: ImapProtocol,
    pub current_mailbox: MailboxSelection,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum MailboxSelection {
    None,
    Select(MailboxHash),
    Examine(MailboxHash),
}

impl MailboxSelection {
    pub fn take(&mut self) -> Self {
        std::mem::replace(self, Self::None)
    }
}

async fn try_await(cl: impl Future<Output = Result<()>> + Send) -> Result<()> {
    cl.await
}

#[derive(Debug)]
pub struct ImapConnection {
    #[cfg(debug_assertions)]
    pub id: Cow<'static, str>,
    pub stream: Result<ImapStream>,
    pub server_conf: ImapServerConf,
    pub sync_policy: SyncPolicy,
    pub uid_store: Arc<UIDStore>,
}

impl ImapStream {
    pub async fn new_connection(
        server_conf: &ImapServerConf,
        #[cfg(debug_assertions)] id: Cow<'static, str>,
        uid_store: &UIDStore,
    ) -> Result<(Capabilities, Self)> {
        let path = &server_conf.server_hostname;

        let cmd_id = 1;
        let stream = if server_conf.use_tls {
            (uid_store.event_consumer)(
                uid_store.account_hash,
                BackendEvent::AccountStateChange {
                    message: "Establishing TLS connection.".into(),
                },
            );
            let mut connector = TlsConnector::builder();
            if server_conf.danger_accept_invalid_certs {
                connector.danger_accept_invalid_certs(true);
            }
            let connector = connector
                .build()
                .chain_err_kind(ErrorKind::Network(NetworkErrorKind::InvalidTLSConnection))?;

            let addr = (path.as_str(), server_conf.server_port);

            let mut socket = AsyncWrapper::new({
                let conn = Connection::new_tcp(tcp_stream_connect(addr, server_conf.timeout)?);
                #[cfg(feature = "imap-trace")]
                let conn = conn.trace(true).with_id("imap");
                conn
            })?;
            if server_conf.use_starttls {
                let err_fn = || {
                    if server_conf.server_port == 993 {
                        "STARTTLS failed. Server port is set to 993, which normally uses TLS. \
                         Maybe try disabling use_starttls."
                    } else {
                        "STARTTLS failed. Is the connection already encrypted?"
                    }
                };
                let mut buf = vec![0; Connection::IO_BUF_SIZE];
                match server_conf.protocol {
                    ImapProtocol::IMAP { .. } => socket
                        .write_all(format!("M{} STARTTLS\r\n", cmd_id).as_bytes())
                        .await
                        .chain_err_summary(err_fn)?,
                    ImapProtocol::ManageSieve => {
                        socket.read(&mut buf).await.chain_err_summary(err_fn)?;
                        socket
                            .write_all(b"STARTTLS\r\n")
                            .await
                            .chain_err_summary(err_fn)?;
                    }
                }
                socket.flush().await.chain_err_summary(err_fn)?;
                let mut response = Vec::with_capacity(1024);
                let mut broken = false;
                let now = Instant::now();

                while now.elapsed().as_secs() < 3 {
                    let len = socket.read(&mut buf).await.chain_err_summary(err_fn)?;
                    response.extend_from_slice(&buf[0..len]);
                    match server_conf.protocol {
                        ImapProtocol::IMAP { .. } => {
                            if response.starts_with(b"* OK ") && response.find(b"\r\n").is_some() {
                                if let Some(pos) = response.find(b"\r\n") {
                                    response.drain(0..pos + 2);
                                }
                            }
                        }
                        ImapProtocol::ManageSieve => {
                            if response.starts_with(b"OK ") && response.find(b"\r\n").is_some() {
                                response.clear();
                                broken = true;
                                break;
                            }
                        }
                    }
                    if response.starts_with(b"M1 OK") {
                        broken = true;
                        break;
                    }
                }
                if !broken {
                    return Err(Error::new(format!(
                        "Could not initiate STARTTLS negotiation to {}.",
                        path
                    )));
                }
            }

            {
                // [ref:FIXME]: This is blocking
                let socket = socket.into_inner()?;
                let mut conn_result = connector.connect(path, socket);
                if let Err(native_tls::HandshakeError::WouldBlock(midhandshake_stream)) =
                    conn_result
                {
                    let mut midhandshake_stream = Some(midhandshake_stream);
                    loop {
                        match midhandshake_stream.take().unwrap().handshake() {
                            Ok(r) => {
                                conn_result = Ok(r);
                                break;
                            }
                            Err(native_tls::HandshakeError::WouldBlock(stream)) => {
                                midhandshake_stream = Some(stream);
                            }
                            p => {
                                p.chain_err_kind(ErrorKind::Network(
                                    NetworkErrorKind::InvalidTLSConnection,
                                ))?;
                            }
                        }
                    }
                }
                AsyncWrapper::new(Connection::new_tls(conn_result.chain_err_summary(
                    || format!("Could not initiate TLS negotiation to {}.", path),
                )?))
                .chain_err_summary(|| format!("Could not initiate TLS negotiation to {}.", path))?
            }
        } else {
            let addr = (path.as_str(), server_conf.server_port);
            AsyncWrapper::new({
                let conn = Connection::new_tcp(tcp_stream_connect(addr, server_conf.timeout)?);
                #[cfg(feature = "imap-trace")]
                let conn = conn.trace(true).with_id("imap");
                conn
            })?
        };
        if let Err(err) = stream
            .get_ref()
            .set_keepalive(Some(Duration::new(60 * 9, 0)))
        {
            log::warn!("Could not set TCP keepalive in IMAP connection: {}", err);
        }
        let mut res = Vec::with_capacity(8 * 1024);
        let mut ret = Self {
            cmd_id,
            #[cfg(debug_assertions)]
            id,
            stream,
            protocol: server_conf.protocol,
            current_mailbox: MailboxSelection::None,
            timeout: server_conf.timeout,
        };
        if matches!(server_conf.protocol, ImapProtocol::ManageSieve) {
            ret.read_response(&mut res).await?;
            let credentials = format!(
                "\0{}\0{}",
                &server_conf.server_username, &server_conf.server_password
            );
            ret.send_command(CommandBody::authenticate_with_ir(
                AuthMechanism::Plain,
                credentials.as_bytes(),
            ))
            .await?;
            ret.read_response(&mut res).await?;
            return Ok((Default::default(), ret));
        }

        (uid_store.event_consumer)(
            uid_store.account_hash,
            BackendEvent::AccountStateChange {
                message: "Negotiating server capabilities.".into(),
            },
        );
        ret.send_command(CommandBody::Capability).await?;
        ret.read_response(&mut res).await?;
        let capabilities: std::result::Result<Vec<&[u8]>, _> = res
            .split_rn()
            .find(|l| l.starts_with(b"* CAPABILITY"))
            .ok_or_else(|| Error::new(""))
            .and_then(|res| {
                protocol_parser::capabilities(res)
                    .map_err(|_| Error::new(""))
                    .map(|(_, v)| v)
            });

        if capabilities.is_err() {
            return Err(Error::new(format!(
                "Could not connect to {}: expected CAPABILITY response but got:{}",
                &server_conf.server_hostname,
                String::from_utf8_lossy(&res)
            ))
            .set_kind(ErrorKind::ProtocolError));
        }

        let capabilities = capabilities.unwrap();
        if !capabilities
            .iter()
            .any(|cap| cap.eq_ignore_ascii_case(b"IMAP4rev1"))
        {
            return Err(Error::new(format!(
                "Could not connect to {}: server is not IMAP4rev1 compliant",
                &server_conf.server_hostname
            ))
            .set_kind(ErrorKind::ProtocolNotSupported));
        } else if capabilities
            .iter()
            .any(|cap| cap.eq_ignore_ascii_case(b"LOGINDISABLED"))
        {
            return Err(Error::new(format!(
                "Could not connect to {}: server does not accept logins [LOGINDISABLED]",
                &server_conf.server_hostname
            ))
            .set_err_kind(ErrorKind::Authentication));
        }

        (uid_store.event_consumer)(
            uid_store.account_hash,
            BackendEvent::AccountStateChange {
                message: "Attempting authentication.".into(),
            },
        );
        match server_conf.protocol {
            ImapProtocol::IMAP {
                extension_use: ImapExtensionUse { oauth2, .. },
            } if oauth2 => {
                if !capabilities
                    .iter()
                    .any(|cap| cap.eq_ignore_ascii_case(b"AUTH=XOAUTH2"))
                {
                    return Err(Error::new(format!(
                        "Could not connect to {}: OAUTH2 is enabled but server did not return \
                         AUTH=XOAUTH2 capability. Returned capabilities were: {}",
                        &server_conf.server_hostname,
                        capabilities
                            .iter()
                            .map(|capability| String::from_utf8_lossy(capability).to_string())
                            .collect::<Vec<String>>()
                            .join(" ")
                    ))
                    .set_err_kind(ErrorKind::Authentication));
                }
                let xoauth2 = base64::decode(&server_conf.server_password)
                    .chain_err_summary(|| {
                        "Could not decode `server_password` from base64. Is the value correct?"
                    })
                    .chain_err_kind(ErrorKind::Configuration)?;
                ret.send_command(CommandBody::authenticate_with_ir(
                    AuthMechanism::XOAuth2,
                    &xoauth2,
                ))
                .await?;
            }
            _ => {
                let username = AString::try_from(server_conf.server_username.as_str())
                    .chain_err_kind(ErrorKind::Bug)?;
                let password = AString::try_from(server_conf.server_password.as_str())
                    .chain_err_kind(ErrorKind::Bug)?;

                ret.send_command(CommandBody::Login {
                    username,
                    password: Secret::new(password),
                })
                .await?;
            }
        }
        let tag_start = format!("M{} ", (ret.cmd_id - 1));
        let mut capabilities = None;

        loop {
            ret.read_lines(&mut res, &[], false).await?;
            let mut should_break = false;
            for l in res.split_rn() {
                if l.starts_with(b"* CAPABILITY") {
                    capabilities = protocol_parser::capabilities(l)
                        .map(|(_, capabilities)| {
                            HashSet::from_iter(capabilities.into_iter().map(|s: &[u8]| s.to_vec()))
                        })
                        .ok();
                }

                if l.starts_with(tag_start.as_bytes()) {
                    if !l[tag_start.len()..].trim().starts_with(b"OK ") {
                        return Err(Error::new(format!(
                            "Could not connect. Server replied with '{}'",
                            String::from_utf8_lossy(l[tag_start.len()..].trim())
                        ))
                        .set_err_kind(ErrorKind::Authentication));
                    }
                    should_break = true;
                }
            }
            if should_break {
                break;
            }
        }

        if capabilities.is_none() {
            /* sending CAPABILITY after LOGIN automatically is an RFC recommendation, so
             * check for lazy servers */
            ret.send_command(CommandBody::Capability).await?;
            ret.read_response(&mut res).await.unwrap();
            let capabilities = protocol_parser::capabilities(&res)?.1;
            let capabilities = HashSet::from_iter(capabilities.into_iter().map(|s| s.to_vec()));
            Ok((capabilities, ret))
        } else {
            let capabilities = capabilities.unwrap();
            Ok((capabilities, ret))
        }
    }

    pub async fn read_response(&mut self, ret: &mut Vec<u8>) -> Result<()> {
        let id = match self.protocol {
            ImapProtocol::IMAP { .. } => format!("M{} ", self.cmd_id - 1).into_bytes(),
            ImapProtocol::ManageSieve => Vec::new(),
        };
        self.read_lines(ret, &id, true).await?;
        Ok(())
    }

    pub async fn read_lines(
        &mut self,
        ret: &mut Vec<u8>,
        termination_string: &[u8],
        keep_termination_string: bool,
    ) -> Result<()> {
        let mut buf: Vec<u8> = vec![0; Connection::IO_BUF_SIZE];
        ret.clear();
        let mut last_line_idx: usize = 0;
        loop {
            match timeout(self.timeout, self.stream.read(&mut buf)).await? {
                Ok(0) => break,
                Ok(b) => {
                    ret.extend_from_slice(&buf[0..b]);
                    if let Some(mut pos) = ret[last_line_idx..].rfind("\r\n") {
                        if ret[last_line_idx..].starts_with(b"* BYE") {
                            return Err(Error::new("Disconnected"));
                        }
                        if let Some(prev_line) =
                            ret[last_line_idx..pos + last_line_idx].rfind(b"\r\n")
                        {
                            last_line_idx += prev_line + b"\r\n".len();
                            pos -= prev_line + b"\r\n".len();
                        }
                        if Some(pos + b"\r\n".len()) == ret.get(last_line_idx..).map(|r| r.len()) {
                            if !termination_string.is_empty()
                                && ret[last_line_idx..].starts_with(termination_string)
                            {
                                if !keep_termination_string {
                                    ret.splice(last_line_idx.., std::iter::empty::<u8>());
                                }
                                break;
                            } else if termination_string.is_empty() {
                                break;
                            }
                        }
                        last_line_idx += pos + b"\r\n".len();
                    }
                }
                Err(err) => {
                    return Err(Error::from(err));
                }
            }
        }
        //imap_trace!(self, "returning IMAP response:\n{:?}", &ret);
        Ok(())
    }

    pub async fn wait_for_continuation_request(&mut self) -> Result<()> {
        let term = b"+ ";
        let mut ret = Vec::new();
        self.read_lines(&mut ret, &term[..], false).await?;
        Ok(())
    }

    pub async fn send_command(&mut self, body: CommandBody<'_>) -> Result<()> {
        timeout(self.timeout, async {
            let command = {
                let tag = Tag::unvalidated(format!("M{}", self.cmd_id));

                Command { tag, body }
            };
            match self.protocol {
                ImapProtocol::IMAP { .. } => {
                    if matches!(command.body, CommandBody::Login { .. }) {
                        imap_trace!(self, "sent: M{} LOGIN ..", self.cmd_id - 1);
                    } else {
                        imap_trace!(self, "sent: M{} {:?}", self.cmd_id - 1, command.body);
                    }
                }
                ImapProtocol::ManageSieve => {}
            }

            for action in CommandCodec::default().encode(&command) {
                match action {
                    Fragment::Line { data } => {
                        self.stream.write_all(&data).await?;
                    }
                    Fragment::Literal { data, mode } => {
                        // We only need to wait for a continuation request when we are about to
                        // send a synchronizing literal, i.e., when not using LITERAL+.
                        if mode == LiteralMode::Sync {
                            self.wait_for_continuation_request().await?;
                        }
                        self.stream.write_all(&data).await?;
                    }
                }
                // Note: This is required for compression to work...
                self.stream.flush().await?;
            }

            self.cmd_id += 1;

            Ok(())
        })
        .await?
    }

    pub async fn send_command_raw(&mut self, command: &[u8]) -> Result<()> {
        _ = timeout(
            self.timeout,
            try_await(async move {
                let command = command.trim();
                match self.protocol {
                    ImapProtocol::IMAP { .. } => {
                        self.stream.write_all(b"M").await?;
                        self.stream
                            .write_all(self.cmd_id.to_string().as_bytes())
                            .await?;
                        self.stream.write_all(b" ").await?;
                        self.cmd_id += 1;
                    }
                    ImapProtocol::ManageSieve => {}
                }

                self.stream.write_all(command).await?;
                self.stream.write_all(b"\r\n").await?;
                self.stream.flush().await?;
                match self.protocol {
                    ImapProtocol::IMAP { .. } => {
                        if !command.starts_with(b"LOGIN") {
                            imap_trace!(self, "sent: M{} {}", self.cmd_id - 1, unsafe {
                                std::str::from_utf8_unchecked(command)
                            });
                        } else {
                            imap_trace!(self, "sent: M{} LOGIN ..", self.cmd_id - 1);
                        }
                    }
                    ImapProtocol::ManageSieve => {}
                }
                Ok(())
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn send_literal(&mut self, data: &[u8]) -> Result<()> {
        self.stream.write_all(data).await?;
        self.stream.write_all(b"\r\n").await?;
        self.stream.flush().await?;
        Ok(())
    }

    pub async fn send_raw(&mut self, raw: &[u8]) -> Result<()> {
        self.stream.write_all(raw).await?;
        self.stream.write_all(b"\r\n").await?;
        self.stream.flush().await?;
        Ok(())
    }
}

impl ImapConnection {
    pub fn new_connection(
        server_conf: &ImapServerConf,
        #[cfg(debug_assertions)] id: Cow<'static, str>,
        uid_store: Arc<UIDStore>,
    ) -> Self {
        Self {
            stream: Err(Error::new("Offline".to_string())),
            #[cfg(debug_assertions)]
            id,
            server_conf: server_conf.clone(),
            sync_policy: if *uid_store.keep_offline_cache.lock().unwrap() {
                SyncPolicy::Basic
            } else {
                SyncPolicy::None
            },
            uid_store,
        }
    }

    pub fn connect<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let (time, ref mut status @ Ok(())) = *self.uid_store.is_online.lock().unwrap() {
                if SystemTime::now().duration_since(time).unwrap_or_default()
                    >= IMAP_PROTOCOL_TIMEOUT
                {
                    let err = Error::new(format!(
                        "Connection timed out after {} seconds",
                        IMAP_PROTOCOL_TIMEOUT.as_secs()
                    ))
                    .set_kind(ErrorKind::TimedOut);
                    *status = Err(err.clone());
                    self.stream = Err(err);
                }
            }
            if self.stream.is_ok() {
                let mut ret = Vec::new();
                if let Err(_err) = try_await(async {
                    self.send_command(CommandBody::Noop).await?;
                    self.read_response(&mut ret, RequiredResponses::empty())
                        .await
                })
                .await
                {
                    imap_trace!(self, "connect(): connection is probably dead: {:?}", &_err);
                } else {
                    imap_trace!(
                        self,
                        "connect(): connection is probably alive, NOOP returned {:?}",
                        &String::from_utf8_lossy(&ret)
                    );
                    return Ok(());
                }
            }
            let new_stream = ImapStream::new_connection(
                &self.server_conf,
                #[cfg(debug_assertions)]
                self.id.clone(),
                &self.uid_store,
            )
            .await;
            if let Err(err) = new_stream.as_ref() {
                self.uid_store.is_online.lock().unwrap().1 = Err(err.clone());
            } else {
                *self.uid_store.is_online.lock().unwrap() = (SystemTime::now(), Ok(()));
            }
            let (capabilities, stream) = new_stream?;
            self.stream = Ok(stream);
            match self.stream.as_ref()?.protocol {
                ImapProtocol::IMAP {
                    extension_use:
                        ImapExtensionUse {
                            condstore,
                            deflate,
                            idle: _idle,
                            oauth2: _,
                        },
                } => {
                    if capabilities.contains(&b"CONDSTORE"[..]) && condstore {
                        match self.sync_policy {
                            SyncPolicy::None => { /* do nothing, sync is disabled */ }
                            _ => {
                                /* Upgrade to Condstore */
                                let mut ret = Vec::new();
                                if capabilities.contains(&b"ENABLE"[..]) {
                                    self.send_command(CommandBody::Enable {
                                        capabilities: NonEmptyVec::from(
                                            CapabilityEnable::CondStore,
                                        ),
                                    })
                                    .await?;
                                    self.read_response(&mut ret, RequiredResponses::empty())
                                        .await?;
                                } else {
                                    self.send_command(CommandBody::Status {
                                        mailbox: Mailbox::Inbox,
                                        item_names: vec![
                                            StatusDataItemName::UidNext,
                                            StatusDataItemName::UidValidity,
                                            StatusDataItemName::Unseen,
                                            StatusDataItemName::Messages,
                                            StatusDataItemName::HighestModSeq,
                                        ]
                                        .into(),
                                    })
                                    .await?;
                                    self.read_response(&mut ret, RequiredResponses::empty())
                                        .await?;
                                }
                                self.sync_policy = SyncPolicy::Condstore;
                            }
                        }
                    }
                    if capabilities.contains(&b"COMPRESS=DEFLATE"[..]) && deflate {
                        let mut ret = Vec::new();
                        self.send_command(CommandBody::compress(CompressionAlgorithm::Deflate))
                            .await?;
                        self.read_response(&mut ret, RequiredResponses::empty())
                            .await?;
                        match ImapResponse::try_from(ret.as_slice())? {
                            ImapResponse::No(code)
                            | ImapResponse::Bad(code)
                            | ImapResponse::Preauth(code)
                            | ImapResponse::Bye(code) => {
                                log::warn!(
                                    "Could not use COMPRESS=DEFLATE in account `{}`: server \
                                     replied with `{}`",
                                    self.uid_store.account_name,
                                    code
                                );
                            }
                            ImapResponse::Ok(_) => {
                                let ImapStream {
                                    cmd_id,
                                    #[cfg(debug_assertions)]
                                    id,
                                    stream,
                                    protocol,
                                    current_mailbox,
                                    timeout,
                                } = std::mem::replace(&mut self.stream, Err(Error::new("")))?;
                                let stream = stream.into_inner()?;
                                self.stream = Ok(ImapStream {
                                    cmd_id,
                                    #[cfg(debug_assertions)]
                                    id,
                                    stream: AsyncWrapper::new(stream.deflate())?,
                                    protocol,
                                    current_mailbox,
                                    timeout,
                                });
                            }
                        }
                    }
                }
                ImapProtocol::ManageSieve => {}
            }
            *self.uid_store.capabilities.lock().unwrap() = capabilities;
            Ok(())
        })
    }

    pub fn read_response<'a>(
        &'a mut self,
        ret: &'a mut Vec<u8>,
        required_responses: RequiredResponses,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut response = Vec::new();
            ret.clear();
            self.stream.as_mut()?.read_response(&mut response).await?;
            *self.uid_store.is_online.lock().unwrap() = (SystemTime::now(), Ok(()));

            match self.server_conf.protocol {
                ImapProtocol::IMAP { .. } => {
                    let r: ImapResponse = ImapResponse::try_from(response.as_slice())?;
                    match r {
                        ImapResponse::Bye(ref response_code) => {
                            self.stream = Err(Error::new(format!(
                                "Offline: received BYE: {:?}",
                                response_code
                            )));
                            ret.extend_from_slice(&response);
                            return r.into();
                        }
                        ImapResponse::No(ref _response_code)
                            if required_responses.intersects(RequiredResponses::NO_REQUIRED) =>
                        {
                            imap_trace!(
                                self,
                                "Received expected NO response: {:?} {:?}",
                                _response_code,
                                String::from_utf8_lossy(&response)
                            );
                        }
                        ImapResponse::No(ref response_code) => {
                            imap_trace!(
                                self,
                                "Received NO response: {:?} {:?}",
                                response_code,
                                String::from_utf8_lossy(&response)
                            );
                            (self.uid_store.event_consumer)(
                                self.uid_store.account_hash,
                                BackendEvent::Notice {
                                    description: response_code.to_string(),
                                    content: None,
                                    level: LogLevel::ERROR,
                                },
                            );
                            ret.extend_from_slice(&response);
                            return r.into();
                        }
                        ImapResponse::Bad(ref response_code) => {
                            imap_trace!(
                                self,
                                "Received BAD response: {:?} {:?}",
                                response_code,
                                String::from_utf8_lossy(&response)
                            );
                            (self.uid_store.event_consumer)(
                                self.uid_store.account_hash,
                                BackendEvent::Notice {
                                    description: response_code.to_string(),
                                    content: None,
                                    level: LogLevel::ERROR,
                                },
                            );
                            ret.extend_from_slice(&response);
                            return r.into();
                        }
                        _ => {}
                    }
                    /* imap_trace!(self,
                        "check every line for required_responses: {:#?}",
                        &required_responses
                    );*/
                    for l in response.split_rn() {
                        /* imap_trace!(self, "check line: {}", &l); */
                        if required_responses.check(l) || !self.process_untagged(l).await? {
                            ret.extend_from_slice(l);
                        }
                    }
                    Ok(())
                }
                ImapProtocol::ManageSieve => {
                    ret.extend_from_slice(&response);
                    Ok(())
                }
            }
        })
    }

    pub async fn read_lines(
        &mut self,
        ret: &mut Vec<u8>,
        termination_string: Vec<u8>,
    ) -> Result<()> {
        self.stream
            .as_mut()?
            .read_lines(ret, &termination_string, false)
            .await?;
        Ok(())
    }

    pub async fn wait_for_continuation_request(&mut self) -> Result<()> {
        self.stream
            .as_mut()?
            .wait_for_continuation_request()
            .await?;
        Ok(())
    }

    pub async fn send_command(&mut self, command: CommandBody<'_>) -> Result<()> {
        if let Err(err) =
            try_await(async { self.stream.as_mut()?.send_command(command).await }).await
        {
            self.stream = Err(err.clone());
            if err.kind.is_network() {
                self.connect().await?;
            }
            Err(err)
        } else {
            *self.uid_store.is_online.lock().unwrap() = (SystemTime::now(), Ok(()));
            Ok(())
        }
    }

    pub async fn send_command_raw(&mut self, command: &[u8]) -> Result<()> {
        if let Err(err) =
            try_await(async { self.stream.as_mut()?.send_command_raw(command).await }).await
        {
            self.stream = Err(err.clone());
            if err.kind.is_network() {
                self.connect().await?;
            }
            Err(err)
        } else {
            *self.uid_store.is_online.lock().unwrap() = (SystemTime::now(), Ok(()));
            Ok(())
        }
    }

    pub async fn send_literal(&mut self, data: &[u8]) -> Result<()> {
        if let Err(err) = try_await(async { self.stream.as_mut()?.send_literal(data).await }).await
        {
            self.stream = Err(err.clone());
            if err.kind.is_network() {
                self.connect().await?;
            }
            Err(err)
        } else {
            Ok(())
        }
    }

    pub async fn send_raw(&mut self, raw: &[u8]) -> Result<()> {
        if let Err(err) = try_await(async { self.stream.as_mut()?.send_raw(raw).await }).await {
            self.stream = Err(err.clone());
            if err.kind.is_network() {
                self.connect().await?;
            }
            Err(err)
        } else {
            Ok(())
        }
    }

    pub async fn select_mailbox(
        &mut self,
        mailbox_hash: MailboxHash,
        ret: &mut Vec<u8>,
        force: bool,
    ) -> Result<Option<SelectResponse>> {
        if !force && self.stream.as_ref()?.current_mailbox == MailboxSelection::Select(mailbox_hash)
        {
            return Ok(None);
        }
        let (imap_path, no_select, permissions) = {
            let m = &self.uid_store.mailboxes.lock().await[&mailbox_hash];
            (
                m.imap_path().to_string(),
                m.no_select,
                m.permissions.clone(),
            )
        };
        if no_select {
            return Err(Error::new(format!(
                "Trying to select a \\NoSelect mailbox: {}",
                &imap_path
            ))
            .set_kind(ErrorKind::Bug));
        }
        self.send_command(CommandBody::select(imap_path.as_str())?)
            .await?;
        self.read_response(ret, RequiredResponses::SELECT_REQUIRED)
            .await?;
        imap_trace!(
            self,
            "{} SELECT response {}",
            imap_path,
            String::from_utf8_lossy(ret)
        );
        let select_response = protocol_parser::select_response(ret).chain_err_summary(|| {
            format!("Could not parse select response for mailbox {}", imap_path)
        })?;
        {
            if *self.uid_store.keep_offline_cache.lock().unwrap() {
                #[cfg(not(feature = "sqlite3"))]
                let mut cache_handle = super::cache::DefaultCache::get(self.uid_store.clone())?;
                #[cfg(feature = "sqlite3")]
                let mut cache_handle = super::cache::Sqlite3Cache::get(self.uid_store.clone())?;
                if let Err(err) = cache_handle.mailbox_state(mailbox_hash).and_then(|r| {
                    if r.is_none() {
                        cache_handle.clear(mailbox_hash, &select_response)
                    } else {
                        Ok(())
                    }
                }) {
                    (self.uid_store.event_consumer)(
                        self.uid_store.account_hash,
                        BackendEvent::from(err),
                    );
                }
            }
            self.uid_store
                .mailboxes
                .lock()
                .await
                .entry(mailbox_hash)
                .and_modify(|entry| {
                    *entry.select.write().unwrap() = Some(select_response.clone());
                });
        }
        {
            let mut permissions = permissions.lock().unwrap();
            permissions.create_messages = !select_response.read_only;
            permissions.remove_messages = !select_response.read_only;
            permissions.set_flags = !select_response.read_only;
            permissions.rename_messages = !select_response.read_only;
            permissions.delete_messages = !select_response.read_only;
        }
        self.stream.as_mut()?.current_mailbox = MailboxSelection::Select(mailbox_hash);
        if self
            .uid_store
            .msn_index
            .lock()
            .unwrap()
            .get(&mailbox_hash)
            .map(|i| i.is_empty())
            .unwrap_or(true)
        {
            self.create_uid_msn_cache(mailbox_hash, 1, &select_response)
                .await?;
        }
        Ok(Some(select_response))
    }

    pub async fn examine_mailbox(
        &mut self,
        mailbox_hash: MailboxHash,
        ret: &mut Vec<u8>,
        force: bool,
    ) -> Result<Option<SelectResponse>> {
        if !force
            && self.stream.as_ref()?.current_mailbox == MailboxSelection::Examine(mailbox_hash)
        {
            return Ok(None);
        }
        let (imap_path, no_select) = {
            let m = &self.uid_store.mailboxes.lock().await[&mailbox_hash];
            (m.imap_path().to_string(), m.no_select)
        };
        if no_select {
            return Err(Error::new(format!(
                "Trying to examine a \\NoSelect mailbox: {}",
                &imap_path
            ))
            .set_kind(ErrorKind::Bug));
        }
        self.send_command(CommandBody::examine(imap_path.as_str())?)
            .await?;
        self.read_response(ret, RequiredResponses::EXAMINE_REQUIRED)
            .await?;

        imap_trace!(self, "EXAMINE response {}", String::from_utf8_lossy(ret));
        let select_response = protocol_parser::select_response(ret).chain_err_summary(|| {
            format!("Could not parse select response for mailbox {}", imap_path)
        })?;
        self.stream.as_mut()?.current_mailbox = MailboxSelection::Examine(mailbox_hash);
        if !self
            .uid_store
            .msn_index
            .lock()
            .unwrap()
            .get(&mailbox_hash)
            .map(|i| i.is_empty())
            .unwrap_or(true)
        {
            self.create_uid_msn_cache(mailbox_hash, 1, &select_response)
                .await?;
        }
        Ok(Some(select_response))
    }

    pub async fn unselect(&mut self) -> Result<()> {
        match self.stream.as_mut()?.current_mailbox.take() {
            MailboxSelection::Examine(_) | MailboxSelection::Select(_) => {
                let mut response = Vec::with_capacity(8 * 1024);
                if self
                    .uid_store
                    .capabilities
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|cap| cap.eq_ignore_ascii_case(b"UNSELECT"))
                {
                    self.send_command(CommandBody::Unselect).await?;
                    self.read_response(&mut response, RequiredResponses::empty())
                        .await?;
                } else {
                    /* `RFC3691 - UNSELECT Command` states: "[..] IMAP4 provides this
                     * functionality (via a SELECT command with a nonexistent mailbox name or
                     * reselecting the same mailbox with EXAMINE command)[..]
                     */
                    let mut nonexistent = "blurdybloop".to_string();
                    {
                        let mailboxes = self.uid_store.mailboxes.lock().await;
                        while mailboxes.values().any(|m| m.imap_path() == nonexistent) {
                            nonexistent.push('p');
                        }
                    }
                    self.send_command(CommandBody::select(nonexistent)?).await?;
                    self.read_response(&mut response, RequiredResponses::NO_REQUIRED)
                        .await?;
                }
            }
            MailboxSelection::None => {}
        }
        Ok(())
    }

    pub fn add_refresh_event(&mut self, ev: RefreshEvent) {
        (self.uid_store.event_consumer)(self.uid_store.account_hash, BackendEvent::Refresh(ev));
    }

    async fn create_uid_msn_cache(
        &mut self,
        mailbox_hash: MailboxHash,
        low: usize,
        _select_response: &SelectResponse,
    ) -> Result<()> {
        debug_assert!(low > 0);
        self.send_command(CommandBody::search(
            None,
            SearchKey::SequenceSet(SequenceSet::try_from(low..)?),
            true,
        ))
        .await?;

        let mut response = Vec::new();
        self.read_response(&mut response, RequiredResponses::SEARCH)
            .await?;
        let mut msn_index_lck = self.uid_store.msn_index.lock().unwrap();
        let msn_index = msn_index_lck.entry(mailbox_hash).or_default();
        let _ = msn_index.drain(low - 1..);
        msn_index.extend(protocol_parser::search_results(&response)?.1.into_iter());
        Ok(())
    }
}

pub struct ImapBlockingConnection {
    buf: Vec<u8>,
    result: Vec<u8>,
    prev_res_length: usize,
    pub conn: ImapConnection,
    err: Option<Error>,
}

impl From<ImapConnection> for ImapBlockingConnection {
    fn from(conn: ImapConnection) -> Self {
        Self {
            buf: vec![0; Connection::IO_BUF_SIZE],
            conn,
            prev_res_length: 0,
            result: Vec::with_capacity(8 * 1024),
            err: None,
        }
    }
}

impl ImapBlockingConnection {
    pub fn into_conn(self) -> ImapConnection {
        self.conn
    }

    pub fn err(&mut self) -> Option<Error> {
        self.err.take()
    }

    pub fn as_stream(&mut self) -> impl Future<Output = Option<Vec<u8>>> + '_ {
        self.result.drain(0..self.prev_res_length);
        self.prev_res_length = 0;
        let mut break_flag = false;
        let mut prev_failure = None;
        async move {
            if self.conn.stream.is_err() {
                return None;
            }
            loop {
                if let Some(y) = read(self, &mut break_flag, &mut prev_failure).await {
                    return Some(y);
                }
                if break_flag {
                    return None;
                }
            }
        }
    }
}

async fn read(
    conn: &mut ImapBlockingConnection,
    break_flag: &mut bool,
    prev_failure: &mut Option<SystemTime>,
) -> Option<Vec<u8>> {
    let ImapBlockingConnection {
        ref mut prev_res_length,
        ref mut result,
        ref mut conn,
        ref mut buf,
        ref mut err,
    } = conn;

    match conn.stream.as_mut().unwrap().stream.read(buf).await {
        Ok(0) => {
            *break_flag = true;
        }
        Ok(b) => {
            result.extend_from_slice(&buf[0..b]);
            if let Some(pos) = result.find(b"\r\n") {
                *prev_res_length = pos + b"\r\n".len();
                return Some(result[0..*prev_res_length].to_vec());
            }
            *prev_failure = None;
        }
        Err(_err) => {
            *err = Some(Into::<Error>::into(_err));
            *break_flag = true;
            *prev_failure = Some(SystemTime::now());
        }
    }
    None
}
