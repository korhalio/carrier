use error::*;
use protocol::Protocol;
use service::{Client, Server};

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Display;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::rc::Rc;

use hole_punch::{plain, Config, Context, FileFormat, PubKey, Stream};

use futures::Async::Ready;
use futures::future::Either;
use futures::{Future, Poll, Stream as FStream};

use tokio_core::reactor::{Core, Handle};

type PeerContextPtr = Rc<RefCell<PeerContext>>;

struct PeerContext {
    services: HashMap<String, Box<Server>>,
}

impl PeerContext {
    fn new() -> PeerContextPtr {
        Rc::new(RefCell::new(PeerContext {
            services: HashMap::new(),
        }))
    }
}

pub struct PeerBuilder {
    config: Config,
    handle: Handle,
    peer_context: PeerContextPtr,
}

impl PeerBuilder {
    fn new(handle: &Handle) -> PeerBuilder {
        let config = Config::new();

        PeerBuilder {
            config,
            handle: handle.clone(),
            peer_context: PeerContext::new(),
        }
    }

    /// Set the TLS certificate filename.
    pub fn set_cert_chain_file<C: Into<PathBuf>>(mut self, path: C) -> PeerBuilder {
        self.config.set_cert_chain_filename(path);
        self
    }

    /// Set the TLS private key filename.
    pub fn set_private_key_file<K: Into<PathBuf>>(mut self, path: K) -> PeerBuilder {
        self.config.set_key_filename(path);
        self
    }

    /// Set the TLS certificate chain for this peer from memory.
    /// This will overwrite any prior call to `set_cert_chain_filename`.
    pub fn set_cert_chain(mut self, chain: Vec<Vec<u8>>, format: FileFormat) -> PeerBuilder {
        self.config.set_cert_chain(chain, format);
        self
    }

    /// Set the TLS private key for this peer from memory.
    /// This will overwrite any prior call to `set_private_key_filename`.
    pub fn set_private_key(mut self, key: Vec<u8>, format: FileFormat) -> PeerBuilder {
        self.config.set_key(key, format);
        self
    }

    /// Register the given service at this peer.
    pub fn register_service<S: Server + 'static>(self, service: S) -> PeerBuilder {
        let name = service.name();
        self.peer_context
            .borrow_mut()
            .services
            .insert(name.into(), Box::new(service));
        self
    }

    /// Set the client CA certificate files.
    /// These CAs will be used to authenticate connecting clients.
    /// When these CAs are not given, all clients will be authenticated successfully.
    pub fn set_client_ca_cert_files(mut self, files: Vec<PathBuf>) -> PeerBuilder {
        self.config.set_client_ca_certificates(files);
        self
    }

    /// Set the server CA certificate files.
    /// These CAs will be used to authenticate servers.
    /// When these CAs are not given, all server will be trusted.
    pub fn set_server_ca_cert_files(mut self, files: Vec<PathBuf>) -> PeerBuilder {
        self.config.set_server_ca_certificates(files);
        self
    }

    /// Connects to the given server and builds the `Peer` instance.
    /// Returns a future that resolves to a `Peer` instance, if the connection could be initiated
    /// successfully.
    pub fn build<A: ToSocketAddrs + Display>(self, server: &A) -> Result<BuildPeer> {
        let server = match server.to_socket_addrs()?.nth(0) {
            Some(addr) => addr,
            None => bail!("Could not resolve any socket address from {}.", server),
        };

        let peer_context = self.peer_context.clone();
        let peer_context2 = self.peer_context;
        let mut context = Context::new(self.handle.clone(), self.config)?;
        let handle = self.handle.clone();
        let handle2 = self.handle;
        let future = context
            .create_connection_to_server(&server)
            .map_err(|e| e.into())
            .and_then(move |s| {
                let mut con = Connection::new(s, peer_context, handle);
                con.send_hello()?;
                Ok(con)
            })
            .map(move |c| Peer::new(handle2, context, peer_context2, c));

        Ok(BuildPeer {
            future: Box::new(future),
        })
    }
}

pub struct BuildPeer {
    future: Box<Future<Item = Peer, Error = Error>>,
}

impl Future for BuildPeer {
    type Item = Peer;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.future.poll()
    }
}

pub struct Peer {
    handle: Handle,
    context: Context<Protocol>,
    peer_context: PeerContextPtr,
    server_con: Connection,
}

impl Peer {
    fn new(
        handle: Handle,
        context: Context<Protocol>,
        peer_context: PeerContextPtr,
        server_con: Connection,
    ) -> Peer {
        Peer {
            handle,
            context,
            peer_context,
            server_con,
        }
    }

    /// Create a `PeerBuilder` for building a `Peer` instance.
    pub fn builder(handle: &Handle) -> PeerBuilder {
        PeerBuilder::new(handle)
    }

    /// Run this `Peer`.
    pub fn run(self, evt_loop: &mut Core) -> Result<()> {
        evt_loop.run(self)
    }

    /// Connect to the given `Peer` and run the given `Service` (locally and remotely).
    pub fn run_service<S: Client>(
        mut self,
        evt_loop: &mut Core,
        service: S,
        peer: PubKey,
    ) -> Result<S::Item>
    where
        Error: From<S::Error>,
    {
        let connection_id = self.context.generate_connection_id();

        let con = self.context.create_connection_to_peer(
            connection_id,
            self.server_con.stream.as_mut().unwrap(),
            Protocol::ConnectToPeer {
                pub_key: peer,
                connection_id,
            },
        )?;

        let con = match evt_loop.run(con.select2(&mut self)).map_err(|e| match e {
            Either::A((e, _)) => e.into(),
            Either::B((e, _)) => e,
        })? {
            Either::A((con, _)) => con,
            Either::B(_) => {
                bail!("connection to server closed while waiting for connection to peer")
            }
        };

        let con = evt_loop.run(RequestService::start(con, service.name())?)?;

        evt_loop
            .run(service.start(&self.handle, con)?)
            .map_err(|e| e.into())
    }
}

impl Future for Peer {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if self.server_con.poll()?.is_ready() {
            return Ok(Ready(()));
        }

        loop {
            match try_ready!(self.context.poll()) {
                Some(stream) => self.handle.spawn(
                    Connection::new(stream, self.peer_context.clone(), self.handle.clone())
                        .map_err(|e| eprintln!("{:?}", e)),
                ),
                None => return Ok(Ready(())),
            }
        }
    }
}

struct Connection {
    context: PeerContextPtr,
    handle: Handle,
    stream: Option<Stream<Protocol>>,
}

impl Connection {
    fn new(stream: Stream<Protocol>, context: PeerContextPtr, handle: Handle) -> Connection {
        Connection {
            stream: Some(stream),
            context,
            handle,
        }
    }

    fn send_hello(&mut self) -> Result<()> {
        self.stream
            .as_mut()
            .unwrap()
            .send_and_poll(Protocol::Hello)
            .map_err(|e| e.into())
    }
}

impl Future for Connection {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let msg = match try_ready!(
                self.stream
                    .as_mut()
                    .expect("can not be polled twice")
                    .poll()
            ) {
                Some(msg) => msg,
                // connection/stream was closed, so we close this connection as well.
                None => return Ok(Ready(())),
            };

            match msg {
                Protocol::RequestService { name } => {
                    if let Some(service) = self.context.borrow_mut().services.get_mut(&name) {
                        self.stream
                            .as_mut()
                            .unwrap()
                            .send_and_poll(Protocol::ServiceConnectionEstablished)?;
                        service.spawn(&self.handle, self.stream.take().unwrap().into_plain())?;
                        return Ok(Ready(()));
                    } else {
                        self.stream
                            .as_mut()
                            .unwrap()
                            .send_and_poll(Protocol::ServiceNotFound)?;
                    }
                }
                Protocol::PeerNotFound => {
                    panic!("Peer not found");
                }
                _ => {}
            }
        }
    }
}

struct RequestService {
    stream: Option<Stream<Protocol>>,
}

impl RequestService {
    fn start(mut stream: Stream<Protocol>, name: &str) -> Result<RequestService> {
        stream.send_and_poll(Protocol::RequestService { name: name.into() })?;
        Ok(RequestService {
            stream: Some(stream),
        })
    }
}

impl Future for RequestService {
    type Item = plain::Stream;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let msg = match try_ready!(
                self.stream
                    .as_mut()
                    .expect("can not be polled twice")
                    .poll()
            ) {
                Some(msg) => msg,
                None => bail!("connection closed while requesting service"),
            };

            match msg {
                Protocol::ServiceNotFound => bail!("service not found"),
                Protocol::ServiceConnectionEstablished => {
                    return Ok(Ready(self.stream.take().unwrap().into_plain()));
                }
                _ => {}
            }
        }
    }
}
