use super::RW;
use super::evented::{Evented, EventedImpl, MioAdapter};
use std::io;
use std::net::SocketAddr;
use mio_orig;
use std;

pub use mio_orig::tcp::Shutdown;

/// TCP Listener
pub type TcpListener = MioAdapter<mio_orig::tcp::TcpListener>;

impl TcpListener {
    /// Local address
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.shared().io_ref().local_addr()
    }

    /// TODO: document
    pub fn take_socket_error(&self) -> io::Result<()> {
        self.shared().io_ref().take_socket_error()
    }

    /// Try cloning the listener descriptor.
    pub fn try_clone(&self) -> io::Result<TcpListener> {
        self.shared().io_ref().try_clone().map(MioAdapter::new)
    }
}

impl TcpListener {
    /// Bind to a port
    pub fn bind(addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpListener::bind(addr).map(MioAdapter::new)
    }

    /// Creates a new TcpListener from an instance of a `std::net::TcpListener` type.
    pub fn from_listener(listener: std::net::TcpListener, addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpListener::from_listener(listener, addr).map(MioAdapter::new)
    }
}

/// TCP Stream
pub type TcpStream = MioAdapter<mio_orig::tcp::TcpStream>;

impl TcpStream {
    /// Create a new TCP stream an issue a non-blocking connect to the specified address.
    pub fn connect(addr: &SocketAddr) -> io::Result<Self> {
        let stream = mio_orig::tcp::TcpStream::connect(addr).map(|t| {
            let stream = MioAdapter::new(t);
            stream.block_on(RW::write());
            stream
        });

        if let Ok(ref stream) = stream {
            if let Err(err) = stream.shared().io_ref().take_socket_error() {
                return Err(err);
            }
        }

        stream

    }

    /// Creates a new TcpStream from the pending socket inside the given
    /// `std::net::TcpBuilder`, connecting it to the address specified.
    pub fn connect_stream(stream: std::net::TcpStream, addr: &SocketAddr) -> io::Result<Self> {
        let stream = mio_orig::tcp::TcpStream::connect_stream(stream, addr).map(|t| {
            let stream = MioAdapter::new(t);
            stream.block_on(RW::write());

            stream
        });

        if let Ok(ref stream) = stream {
            if let Err(err) = stream.shared().io_ref().take_socket_error() {
                return Err(err);
            }
        }

        stream
    }

    /// Local address of connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.shared().io_ref().local_addr()
    }

    /// Peer address of connection.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.shared().io_ref().peer_addr()
    }

    /// Shutdown the connection.
    pub fn shutdown(&self, how: mio_orig::tcp::Shutdown) -> io::Result<()> {
        self.shared().io_ref().shutdown(how)
    }

    /// Set `no_delay`.
    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.shared().io_ref().set_nodelay(nodelay)
    }

    /// Set keepalive.
    pub fn set_keepalive(&self, seconds: Option<u32>) -> io::Result<()> {
        self.shared().io_ref().set_keepalive(seconds)
    }

    /// TODO: document
    pub fn take_socket_error(&self) -> io::Result<()> {
        self.shared().io_ref().take_socket_error()
    }

    /// Try cloning the socket descriptor.
    pub fn try_clone(&self) -> io::Result<TcpStream> {
        self.shared().io_ref().try_clone().map(MioAdapter::new)
    }
}
