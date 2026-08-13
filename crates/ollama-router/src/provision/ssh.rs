//! russh transport: connect, probe, upload script, exec. No OpenSSH binary.

use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, AuthResult, Handle};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};

use super::SshTarget;

/// Host-key check skipped (Python `known_hosts=None`) for bootstrap to changing cloud IPs.
struct AcceptAllKeys;

impl client::Handler for AcceptAllKeys {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Live russh client used in production.
#[derive(Clone, Debug, Default)]
pub struct RusshTransport;

#[derive(Clone, Debug)]
pub struct RemoteOutput {
    pub exit_status: u32,
    pub output: String,
    pub disconnected: bool,
}

impl RusshTransport {
    async fn connect(
        &self,
        target: &SshTarget,
        timeout: Duration,
    ) -> Result<Handle<AcceptAllKeys>, String> {
        let addr = (target.host.as_str(), target.port);
        let stream = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr))
            .await
            .map_err(|_| format!("ssh connect timeout {}:{}", target.host, target.port))?
            .map_err(|err| format!("ssh connect {}:{}: {err}", target.host, target.port))?;
        let config = Arc::new(client::Config::default());
        let mut handle = tokio::time::timeout(
            timeout,
            client::connect_stream(config, stream, AcceptAllKeys),
        )
        .await
        .map_err(|_| "ssh handshake timeout".to_string())?
        .map_err(|err| format!("ssh handshake: {err}"))?;

        authenticate(&mut handle, target).await?;
        Ok(handle)
    }

    pub async fn probe(&self, target: &SshTarget, timeout: Duration) -> Result<(), String> {
        let handle = self.connect(target, timeout).await?;
        let out = exec(&handle, "true", None).await?;
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        if out.exit_status == 0 {
            Ok(())
        } else {
            Err(format!("remote true exit={}", out.exit_status))
        }
    }

    pub async fn run(
        &self,
        target: &SshTarget,
        timeout: Duration,
        command: &str,
        stdin: Option<&[u8]>,
    ) -> Result<RemoteOutput, String> {
        let handle = self.connect(target, timeout).await?;
        let out = exec(&handle, command, stdin).await;
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        out
    }
}

async fn authenticate(
    handle: &mut Handle<AcceptAllKeys>,
    target: &SshTarget,
) -> Result<(), String> {
    if let Some(path) = target.key_file.as_deref() {
        let key = load_secret_key(path, None).map_err(|err| format!("ssh key load: {err}"))?;
        let hash = handle
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten();
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
        match handle
            .authenticate_publickey(target.user.clone(), key)
            .await
            .map_err(|err| format!("ssh publickey: {err}"))?
        {
            AuthResult::Success => return Ok(()),
            AuthResult::Failure { .. } => {
                if target.password.is_none() {
                    return Err("ssh publickey auth failed".into());
                }
            }
        }
    }
    if let Some(password) = target.password.as_deref() {
        match handle
            .authenticate_password(target.user.clone(), password)
            .await
            .map_err(|err| format!("ssh password: {err}"))?
        {
            AuthResult::Success => return Ok(()),
            AuthResult::Failure { .. } => {
                return Err("ssh password auth failed".into());
            }
        }
    }
    if target.key_file.is_none() && target.password.is_none() {
        return Err("ssh key_file or password_env required".into());
    }
    Err("ssh authentication failed".into())
}

async fn exec(
    handle: &Handle<AcceptAllKeys>,
    command: &str,
    stdin: Option<&[u8]>,
) -> Result<RemoteOutput, String> {
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|err| format!("ssh channel: {err}"))?;
    channel
        .exec(true, command)
        .await
        .map_err(|err| format!("ssh exec: {err}"))?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_status = None;
    let mut exec_ok = false;

    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => {
                exec_ok = true;
                if let Some(bytes) = stdin {
                    channel
                        .data(std::io::Cursor::new(bytes.to_vec()))
                        .await
                        .map_err(|err| format!("ssh stdin: {err}"))?;
                    channel
                        .eof()
                        .await
                        .map_err(|err| format!("ssh eof: {err}"))?;
                }
            }
            Some(ChannelMsg::Failure) => {
                return Err("ssh exec rejected".into());
            }
            Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => stderr.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) => {}
            None => break,
            _ => {}
        }
    }

    if stdin.is_some() && !exec_ok {
        return Err("ssh exec did not confirm".into());
    }

    let mut output = String::from_utf8_lossy(&stdout).into_owned();
    if !stderr.is_empty() {
        output.push_str(&String::from_utf8_lossy(&stderr));
    }
    Ok(RemoteOutput {
        exit_status: exit_status.unwrap_or(1),
        output,
        disconnected: false,
    })
}
