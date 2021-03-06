/*!
For running a service over `Carrier`, a `Client` and a `Server` are required.
It is required to implement the given `Client` and `Server` traits for services that should
be running over `Carrier`.

`Carrier` will call `Server::start` whenever a remote `Peer` requests the service from the local
`Peer`. The remote `Peer` needs to run an instance of the `Client` service implementation.
*/
use NewStreamHandle;

use futures::Future;

use std::result;

mod streams;

pub use self::streams::Streams;

pub type ServiceId = u64;

/// Server side of a service.
pub trait Server: Send {
    /// Start a new server instance of the service.
    fn start(&mut self, streams: Streams, new_stream_handle: NewStreamHandle);
    /// Returns the unique name of the service. The name will be used to identify this service.
    fn name(&self) -> &'static str;
}

/// Client side of a service.
pub trait Client: Send {
    type Error: Send;
    type Future: Future<Error = Self::Error> + Send;
    /// Starts a new client instance.
    /// The returned `Future` should resolve, when the service is finished.
    fn start(
        self,
        streams: Streams,
        new_stream_handle: NewStreamHandle,
    ) -> result::Result<Self::Future, Self::Error>;
    /// Returns the unique name of the service. The name will be used to identify this service.
    fn name(&self) -> &'static str;
}
