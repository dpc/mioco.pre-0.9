use super::{RW, RcEvented, Evented};
use super::prv::EventedPrv;
use std::io;
use std::net::SocketAddr;
use super::mio_orig;
use std;
use std::os::unix::io::{RawFd, FromRawFd, AsRawFd};

pub use mio_orig::tcp::Shutdown;

/// TCP Listener
pub struct TcpListener(RcEvented<mio_orig::tcp::TcpListener>);

impl EventedPrv for TcpListener {
    type Raw = mio_orig::tcp::TcpListener;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

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
        self.shared().0.borrow().io.try_clone().map(|t| TcpListener(RcEvented::new(t)))
    }
}

impl FromRawFd for TcpListener {
    unsafe fn from_raw_fd(fd: RawFd) -> TcpListener {
        TcpListener(RcEvented::new(mio_orig::tcp::TcpListener::from_raw_fd(fd)))
    }
}

impl AsRawFd for TcpListener {
    fn as_raw_fd(&self) -> RawFd {
        self.shared().0.borrow_mut().io.as_raw_fd()
    }
}

impl TcpListener {
    /// Bind to a port
    pub fn bind(addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpListener::bind(addr).map(|t| TcpListener(RcEvented::new(t)))
    }

    /// Creates a new TcpListener from an instance of a `std::net::TcpListener` type.
    pub fn from_listener(listener: std::net::TcpListener, addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpListener::from_listener(listener, addr)
            .map(|t| TcpListener(RcEvented::new(t)))
    }

    /// Block on accepting a connection.
    pub fn accept(&self) -> io::Result<TcpStream> {
        loop {
            let res = self.shared().try_accept();

            match res {
                Ok(None) => self.block_on(RW::read()),
                Ok(Some(r)) => {
                    return Ok(TcpStream(RcEvented::new(r)));
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
            .map(|t| t.map(|t| TcpStream(RcEvented::new(t))))

    }
}

unsafe impl Send for TcpListener {}

/// TCP Stream
pub struct TcpStream(RcEvented<mio_orig::tcp::TcpStream>);

impl EventedPrv for TcpStream {
    type Raw = mio_orig::tcp::TcpStream;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl Evented for TcpStream {}


impl FromRawFd for TcpStream {
    unsafe fn from_raw_fd(fd: RawFd) -> TcpStream {
        TcpStream(RcEvented::new(mio_orig::tcp::TcpStream::from_raw_fd(fd)))
    }
}

impl AsRawFd for TcpStream {
    fn as_raw_fd(&self) -> RawFd {
        self.shared().0.borrow_mut().io.as_raw_fd()
    }
}

impl TcpStream {
    /// Create a new TCP stream an issue a non-blocking connect to the specified address.
    pub fn connect(addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpStream::connect(addr).map(|t| TcpStream(RcEvented::new(t)))
    }

    /// Creates a new TcpStream from the pending socket inside the given
    /// `std::net::TcpBuilder`, connecting it to the address specified.
    pub fn connect_stream(stream: std::net::TcpStream, addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpStream::connect_stream(stream, addr).map(|t| TcpStream(RcEvented::new(t)))
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

    /// Try writing a data from the buffer.
    ///
    /// This will not block.
    pub fn try_write(&self, buf: &[u8]) -> io::Result<Option<usize>> {
        self.shared().try_write(buf)
    }

    /// Try cloning the socket descriptor.
    pub fn try_clone(&self) -> io::Result<TcpStream> {
        self.shared().0.borrow().io.try_clone().map(|t| TcpStream(RcEvented::new(t)))
    }
}

impl io::Read for TcpStream {
    /// Block on read.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let res = self.shared().try_read(buf);

            match res {
                Ok(None) => self.block_on(RW::read()),
                Ok(Some(r)) => {
                    return Ok(r);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl TcpStream {
    /// Try reading data into a buffer.
    ///
    /// This will not block.
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.shared().try_read(buf)
    }
}

impl io::Write for TcpStream {
    /// Block on write.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let res = self.shared().try_write(buf);

            match res {
                Ok(None) => self.block_on(RW::write()),
                Ok(Some(r)) => {
                    return Ok(r);
                }
                Err(e) => return Err(e),
            }
        }
    }

    // TODO: ?
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

unsafe impl Send for TcpStream {}
