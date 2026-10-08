//! Authenticated, encrypted transport.
//!
//! * First contact ("pair"): SPAKE2 over a 6-digit code shown on one screen.
//!   A passive eavesdropper learns nothing offline; a wrong guess only gets
//!   one online attempt (the listener burns the code after 3 failures).
//! * Later contacts ("known"): HMAC challenge/response with the 32-byte link
//!   key both sides saved at pairing.
//! * Then ChaCha20-Poly1305 frames with per-direction keys and counters.

use crate::error::{Error, Result};
use crate::proto::*;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};
use tokio::io::{
    split, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};

type HmacSha256 = Hmac<Sha256>;

pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Who we are, as announced in handshakes.
#[derive(Clone, Debug)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub listen_port: u16,
}

/// What we learned about the other side.
#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
    pub listen_port: u16,
    pub purpose: Purpose,
    /// Present only right after a successful *pairing*: persist it.
    pub new_link_key: Option<[u8; 32]>,
}

pub enum ClientAuth {
    Pair { code: String },
    Known { link_key: [u8; 32] },
}

/// Server-side policy hooks (implemented by the node).
pub trait ServerPolicy: Send + Sync {
    fn link_key(&self, peer_id: &str) -> Option<[u8; 32]>;
    /// Current pairing code, if we are accepting new devices.
    fn pairing_code(&self) -> Option<String>;
    fn pair_failed(&self);
    fn pair_succeeded(&self);
}

struct Keys {
    send: [u8; 32],
    recv: [u8; 32],
}

// ---------- framing ----------

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> Result<()> {
    if data.len() > MAX_FRAME {
        return Err(Error::TooLarge);
    }
    let mut buf = Vec::with_capacity(4 + data.len());
    buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
    buf.extend_from_slice(data);
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(Error::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(Error::TooLarge);
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

// ---------- encrypted halves ----------

fn nonce(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    *Nonce::from_slice(&n)
}

pub struct SecureWriter<W> {
    w: W,
    cipher: ChaCha20Poly1305,
    counter: u64,
}

pub struct SecureReader<R> {
    r: R,
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl<W: AsyncWrite + Unpin> SecureWriter<W> {
    pub async fn send<T: serde::Serialize>(&mut self, msg: &T) -> Result<()> {
        let plain = encode(msg);
        let ct = self
            .cipher
            .encrypt(&nonce(self.counter), plain.as_slice())
            .map_err(|_| Error::Auth("encrypt".into()))?;
        self.counter += 1;
        write_frame(&mut self.w, &ct).await
    }
}

impl<W: AsyncWrite + Unpin> SecureWriter<W> {
    /// Encrypts several messages and writes them with a single syscall / packet.
    pub async fn send_many<T: serde::Serialize>(&mut self, msgs: &[T]) -> Result<()> {
        let mut buf = Vec::with_capacity(64 * msgs.len());
        for m in msgs {
            let plain = encode(m);
            let ct = self
                .cipher
                .encrypt(&nonce(self.counter), plain.as_slice())
                .map_err(|_| Error::Auth("encrypt".into()))?;
            self.counter += 1;
            if ct.len() > MAX_FRAME {
                return Err(Error::TooLarge);
            }
            buf.extend_from_slice(&(ct.len() as u32).to_be_bytes());
            buf.extend_from_slice(&ct);
        }
        self.w.write_all(&buf).await?;
        self.w.flush().await?;
        Ok(())
    }
}

impl<R: AsyncRead + Unpin> SecureReader<R> {
    pub async fn recv<T: for<'a> serde::Deserialize<'a>>(&mut self) -> Result<T> {
        let ct = read_frame(&mut self.r, MAX_FRAME).await?;
        let plain = self
            .cipher
            .decrypt(&nonce(self.counter), ct.as_slice())
            .map_err(|_| Error::Auth("bad frame (tampered or desynchronised)".into()))?;
        self.counter += 1;
        decode(&plain)
    }
}

fn make_halves<S: AsyncRead + AsyncWrite + Unpin>(
    s: S,
    keys: Keys,
) -> (
    SecureReader<BufReader<ReadHalf<S>>>,
    SecureWriter<WriteHalf<S>>,
) {
    let (r, w) = split(s);
    let r = BufReader::with_capacity(16 * 1024, r);
    (
        SecureReader {
            r,
            cipher: ChaCha20Poly1305::new(Key::from_slice(&keys.recv)),
            counter: 0,
        },
        SecureWriter {
            w,
            cipher: ChaCha20Poly1305::new(Key::from_slice(&keys.send)),
            counter: 0,
        },
    )
}

// ---------- key schedule ----------

fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, &mut out)
        .expect("32 bytes");
    out
}

fn mac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("any key length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

fn mac_verify(key: &[u8], parts: &[&[u8]], tag: &[u8]) -> bool {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("any key length");
    for p in parts {
        m.update(p);
    }
    m.verify_slice(tag).is_ok()
}

fn transcript(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u32).to_be_bytes());
        h.update(p);
    }
    h.finalize().into()
}

fn normalise_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn session_keys(base: &[u8], nonce_s: &[u8], nonce_c: &[u8], client: bool) -> Keys {
    let salt = [nonce_s, nonce_c].concat();
    let c2s = hkdf32(base, &salt, b"tandem c2s");
    let s2c = hkdf32(base, &salt, b"tandem s2c");
    if client {
        Keys {
            send: c2s,
            recv: s2c,
        }
    } else {
        Keys {
            send: s2c,
            recv: c2s,
        }
    }
}

pub type Secure<S> = (
    SecureReader<BufReader<ReadHalf<S>>>,
    SecureWriter<WriteHalf<S>>,
);

// ---------- client side ----------

pub async fn client_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    mut s: S,
    me: &Identity,
    purpose: Purpose,
    auth: ClientAuth,
) -> Result<(PeerInfo, Secure<S>)> {
    let mode = match auth {
        ClientAuth::Pair { .. } => AuthMode::Pair,
        ClientAuth::Known { .. } => AuthMode::Known,
    };
    let hello = encode(&Hello {
        ver: PROTOCOL_VERSION,
        id: me.id.clone(),
        name: me.name.clone(),
        listen_port: me.listen_port,
        purpose,
        mode,
    });
    write_frame(&mut s, &hello).await?;

    let sh_bytes = read_frame(&mut s, 4096).await?;
    let (srv_id, srv_name, srv_port, nonce_s) = match decode::<ServerHello>(&sh_bytes)? {
        ServerHello::Ok {
            id,
            name,
            listen_port,
            nonce,
        } => (id, name, listen_port, nonce),
        ServerHello::Reject(why) => return Err(Error::Rejected(why)),
    };

    let mut transcript_parts: Vec<Vec<u8>> = vec![hello, sh_bytes];
    let (base, new_link_key) = match auth {
        ClientAuth::Pair { code } => {
            let code = normalise_code(&code);
            let (state, msg_a) = Spake2::<Ed25519Group>::start_a(
                &Password::new(code.as_bytes()),
                &SpakeId::new(me.id.as_bytes()),
                &SpakeId::new(srv_id.as_bytes()),
            );
            write_frame(&mut s, &msg_a).await?;
            let msg_b = read_frame(&mut s, 4096).await?;
            let k = state
                .finish(&msg_b)
                .map_err(|_| Error::Auth("pairing handshake failed".into()))?;
            transcript_parts.push(msg_a);
            transcript_parts.push(msg_b);
            let link = hkdf32(&k, b"tandem link", b"link key");
            (k, Some(link))
        }
        ClientAuth::Known { link_key } => (link_key.to_vec(), None),
    };
    let tr = transcript(
        &transcript_parts
            .iter()
            .map(|v| v.as_slice())
            .collect::<Vec<_>>(),
    );

    let mut nonce_c = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut nonce_c);
    let mac_c = mac(&base, &[b"tandem client", &tr, &nonce_c]);
    let mut m = nonce_c.to_vec();
    m.extend_from_slice(&mac_c);
    write_frame(&mut s, &m).await?;

    let reply = read_frame(&mut s, 4096).await.map_err(|e| match e {
        // Server hangs up on a bad code / bad key without a word.
        Error::Closed => Error::Auth("wrong code or unknown device".into()),
        e => e,
    })?;
    if !mac_verify(&base, &[b"tandem server", &tr, &nonce_c], &reply) {
        return Err(Error::Auth(
            "server could not prove it knows the code".into(),
        ));
    }

    let keys = session_keys(&base, &nonce_s, &nonce_c, true);
    let secure = make_halves(s, keys);
    Ok((
        PeerInfo {
            id: srv_id,
            name: srv_name,
            listen_port: srv_port,
            purpose,
            new_link_key,
        },
        secure,
    ))
}

// ---------- server side ----------

pub async fn server_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    mut s: S,
    me: &Identity,
    policy: &dyn ServerPolicy,
) -> Result<(PeerInfo, Secure<S>)> {
    let hello_bytes = read_frame(&mut s, 4096).await?;
    let hello: Hello = decode(&hello_bytes)?;

    async fn reject<S: AsyncWrite + Unpin>(s: &mut S, why: &str) -> Error {
        let _ = write_frame(s, &encode(&ServerHello::Reject(why.to_string()))).await;
        Error::Auth(why.to_string())
    }

    if hello.ver != PROTOCOL_VERSION {
        return Err(reject(&mut s, "incompatible version").await);
    }
    if hello.id.is_empty() || hello.id.len() > 64 || hello.id == me.id {
        return Err(reject(&mut s, "bad device id").await);
    }

    // Decide how we will authenticate before revealing anything else.
    enum Plan {
        Pair(String),
        Known([u8; 32]),
    }
    let plan = match hello.mode {
        AuthMode::Pair => match policy.pairing_code() {
            Some(c) => Plan::Pair(normalise_code(&c)),
            None => return Err(reject(&mut s, "not accepting new devices").await),
        },
        AuthMode::Known => match policy.link_key(&hello.id) {
            Some(k) => Plan::Known(k),
            None => return Err(reject(&mut s, "unknown device — pair again").await),
        },
    };

    let mut nonce_s = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut nonce_s);
    let sh_bytes = encode(&ServerHello::Ok {
        id: me.id.clone(),
        name: me.name.clone(),
        listen_port: me.listen_port,
        nonce: nonce_s,
    });
    write_frame(&mut s, &sh_bytes).await?;

    let mut transcript_parts: Vec<Vec<u8>> = vec![hello_bytes, sh_bytes];
    let is_pair = matches!(plan, Plan::Pair(_));
    let (base, new_link_key) = match plan {
        Plan::Pair(code) => {
            let msg_a = read_frame(&mut s, 4096).await?;
            let (state, msg_b) = Spake2::<Ed25519Group>::start_b(
                &Password::new(code.as_bytes()),
                &SpakeId::new(hello.id.as_bytes()),
                &SpakeId::new(me.id.as_bytes()),
            );
            write_frame(&mut s, &msg_b).await?;
            let k = match state.finish(&msg_a) {
                Ok(k) => k,
                Err(_) => {
                    policy.pair_failed();
                    return Err(Error::Auth("pairing handshake failed".into()));
                }
            };
            transcript_parts.push(msg_a);
            transcript_parts.push(msg_b);
            let link = hkdf32(&k, b"tandem link", b"link key");
            (k, Some(link))
        }
        Plan::Known(k) => (k.to_vec(), None),
    };
    let tr = transcript(
        &transcript_parts
            .iter()
            .map(|v| v.as_slice())
            .collect::<Vec<_>>(),
    );

    let m = read_frame(&mut s, 4096).await?;
    if m.len() != 64 {
        return Err(Error::Proto("bad confirm".into()));
    }
    let (nonce_c, mac_c) = m.split_at(32);
    if !mac_verify(&base, &[b"tandem client", &tr, nonce_c], mac_c) {
        if is_pair {
            policy.pair_failed();
        }
        // Say nothing: do not help a guesser.
        return Err(Error::Auth("wrong code or key".into()));
    }
    if is_pair {
        policy.pair_succeeded();
    }
    let mac_s = mac(&base, &[b"tandem server", &tr, nonce_c]);
    write_frame(&mut s, &mac_s).await?;

    let keys = session_keys(&base, &nonce_s, nonce_c, false);
    let secure = make_halves(s, keys);
    Ok((
        PeerInfo {
            id: hello.id,
            name: hello.name,
            listen_port: hello.listen_port,
            purpose: hello.purpose,
            new_link_key,
        },
        secure,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    struct Policy {
        code: Option<String>,
        keys: Mutex<Vec<(String, [u8; 32])>>,
        fails: AtomicU32,
    }
    impl ServerPolicy for Policy {
        fn link_key(&self, id: &str) -> Option<[u8; 32]> {
            self.keys
                .lock()
                .unwrap()
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, k)| *k)
        }
        fn pairing_code(&self) -> Option<String> {
            self.code.clone()
        }
        fn pair_failed(&self) {
            self.fails.fetch_add(1, Ordering::SeqCst);
        }
        fn pair_succeeded(&self) {}
    }
    fn ident(id: &str) -> Identity {
        Identity {
            id: id.into(),
            name: id.into(),
            listen_port: 1,
        }
    }
    fn policy(code: Option<&str>) -> Policy {
        Policy {
            code: code.map(String::from),
            keys: Mutex::new(vec![]),
            fails: AtomicU32::new(0),
        }
    }

    #[tokio::test]
    async fn pair_then_known_and_encrypted_roundtrip() {
        let (a, b) = tokio::io::duplex(65536);
        let pol = policy(Some("123456"));
        let srv = tokio::spawn(async move {
            let (peer, (mut r, mut w)) = server_handshake(b, &ident("srv"), &pol).await.unwrap();
            let m: Msg = r.recv().await.unwrap();
            w.send(&Msg::Pong).await.unwrap();
            (peer, m, pol)
        });
        let (peer, (mut r, mut w)) = client_handshake(
            a,
            &ident("cli"),
            Purpose::Control,
            ClientAuth::Pair {
                code: "123-456".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(peer.id, "srv");
        let link = peer.new_link_key.expect("pairing yields a link key");
        w.send(&Msg::Ping).await.unwrap();
        assert_eq!(r.recv::<Msg>().await.unwrap(), Msg::Pong);
        let (speer, m, pol) = srv.await.unwrap();
        assert_eq!(m, Msg::Ping);
        assert_eq!(
            speer.new_link_key,
            Some(link),
            "both sides derive the same link key"
        );

        // Reconnect with the saved key.
        pol.keys.lock().unwrap().push(("cli".into(), link));
        let (a, b) = tokio::io::duplex(65536);
        let srv =
            tokio::spawn(
                async move { server_handshake(b, &ident("srv"), &pol).await.map(|x| x.0) },
            );
        let r = client_handshake(
            a,
            &ident("cli"),
            Purpose::Files,
            ClientAuth::Known { link_key: link },
        )
        .await;
        assert!(r.is_ok());
        let sp = srv.await.unwrap().unwrap();
        assert_eq!(sp.purpose, Purpose::Files);
        assert!(sp.new_link_key.is_none());
    }

    #[tokio::test]
    async fn wrong_code_is_rejected_and_counted() {
        let (a, b) = tokio::io::duplex(65536);
        let pol = std::sync::Arc::new(policy(Some("123456")));
        let p2 = pol.clone();
        let srv =
            tokio::spawn(async move { server_handshake(b, &ident("srv"), &*p2).await.is_err() });
        let r = client_handshake(
            a,
            &ident("cli"),
            Purpose::Control,
            ClientAuth::Pair {
                code: "000000".into(),
            },
        )
        .await;
        assert!(r.is_err());
        assert!(srv.await.unwrap());
        assert_eq!(pol.fails.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_device_and_closed_pairing_are_rejected() {
        let (a, b) = tokio::io::duplex(65536);
        let pol = policy(None);
        tokio::spawn(async move {
            let _ = server_handshake(b, &ident("srv"), &pol).await;
        });
        let r = client_handshake(
            a,
            &ident("cli"),
            Purpose::Control,
            ClientAuth::Known { link_key: [7; 32] },
        )
        .await;
        assert!(matches!(r, Err(Error::Rejected(_))));
    }

    #[tokio::test]
    async fn wrong_link_key_fails() {
        let (a, b) = tokio::io::duplex(65536);
        let pol = policy(None);
        pol.keys.lock().unwrap().push(("cli".into(), [1; 32]));
        tokio::spawn(async move {
            let _ = server_handshake(b, &ident("srv"), &pol).await;
        });
        let r = client_handshake(
            a,
            &ident("cli"),
            Purpose::Control,
            ClientAuth::Known { link_key: [2; 32] },
        )
        .await;
        assert!(matches!(r, Err(Error::Auth(_))));
    }
}
