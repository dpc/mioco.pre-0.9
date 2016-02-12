use super::{RW, RcEvented, Evented, EventedPrv, MioAdapter};
use std::io;
use std::net::SocketAddr;
use super::mio_orig;
use std;

pub use mio_orig::tcp::Shutdown;

/// TCP Listener
pub type TcpListener = MioAdapter<mio_orig::tcp::TcpListener>;

impl TcpListener {
    /// Local address
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.shared().0.borrow().io.local_addr()
    }

    /// TODO: document
    pub fn take_socket_error(&self) -> io::Result<()> {
        self.shared().0.borrow().io.take_socket_error()
    }

    /// Try cloning the listener descriptor.
    pub fn try_clone(&self) -> io::Result<TcpListener> {
        self.shared().0.borrow().io.try_clone().map(|t| MioAdapter(RcEvented::new(t)))
    }
}

impl TcpListener {
    /// Bind to a port
    pub fn bind(addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpListener::bind(addr).map(|t| MioAdapter(RcEvented::new(t)))
    }

    /// Creates a new TcpListener from an instance of a `std::net::TcpListener` type.
    pub fn from_listener(listener: std::net::TcpListener, addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpListener::from_listener(listener, addr)
            .map(|t| MioAdapter(RcEvented::new(t)))
    }

    /// Block on accepting a connection.
    pub fn accept(&self) -> io::Result<TcpStream> {
        loop {
            let res = self.shared().try_accept();

            match res {
                Ok(None) => self.block_on(RW::read()),
                Ok(Some(r)) => {
                    return Ok(MioAdapter(RcEvented::new(r)));
                }
                Err(e) => return Err(e),
            }
        }

    }

    /// Attempt to accept a pending connection.
    ///
    /// This will not block.
    pub fn try_accept(&self) -> io::Result<Option<TcpStream>> {
        self.shared()
            .try_accept()
            .map(|t| t.map(|t| MioAdapter(RcEvented::new(t))))

    }
}

unsafe impl Send for TcpListener {}

/// TCP Stream
pub type TcpStream = MioAdapter<mio_orig::tcp::TcpStream>;

impl TcpStream {
    /// Create a new TCP stream an issue a non-blocking connect to the specified address.
    pub fn connect(addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpStream::connect(addr).map(|t| {
            let stream = MioAdapter(RcEvented::new(t));
            stream.block_on(RW::write());
            stream
        })
    }

    /// Creates a new TcpStream from the pending socket inside the given
    /// `std::net::TcpBuilder`, connecting it to the address specified.
    pub fn connect_stream(stream: std::net::TcpStream, addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpStream::connect_stream(stream, addr).map(|t| {
            let stream = MioAdapter(RcEvented::new(t));
            stream.block_on(RW::write());
            stream
        })
    }

    /// Local address of connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.shared().0.borrow().io.local_addr()
    }

    /// Peer address of connection.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.shared().0.borrow().io.peer_addr()
    }

    /// Shutdown the connection.
    pub fn shutdown(&self, how: mio_orig::tcp::Shutdown) -> io::Result<()> {
        self.shared().0.borrow().io.shutdown(how)
    }

    /// Set `no_delay`.
    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.shared().0.borrow().io.set_nodelay(nodelay)
    }

    /// Set keepalive.
    pub fn set_keepalive(&self, seconds: Option<u32>) -> io::Result<()> {
        self.shared().0.borrow().io.set_keepalive(seconds)
    }

    /// TODO: document
    pub fn take_socket_error(&self) -> io::Result<()> {
        self.shared().0.borrow().io.take_socket_error()
    }

    /// Try cloning the socket descriptor.
    pub fn try_clone(&self) -> io::Result<TcpStream> {
        self.shared().0.borrow().io.try_clone().map(|t| MioAdapter(RcEvented::new(t)))
    }
}

unsafe impl Send for TcpStream {}
