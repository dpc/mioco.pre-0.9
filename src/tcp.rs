use super::{RW, RcEvented, Evented};
use super::prv::EventedPrv;
use std::io;
use std::net::SocketAddr;
use super::mio_orig;

pub use mio_orig::tcp::Shutdown;

/// TCP Listener
pub struct TcpListener(RcEvented<mio_orig::tcp::TcpListener>);

/// TCP Stream
pub struct TcpStream(RcEvented<mio_orig::tcp::TcpStream>);

impl EventedPrv for TcpListener {
    type Raw = mio_orig::tcp::TcpListener;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl EventedPrv for TcpStream {
    type Raw = mio_orig::tcp::TcpStream;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl Evented for TcpStream { }

impl TcpListener {
    /// Local address
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.shared().0.borrow().io.local_addr()
    }

    /// Try cloning the listener descriptor.
    pub fn try_clone(&self) -> io::Result<TcpListener> {
        self.shared().0.borrow().io.try_clone().map(|t| TcpListener(RcEvented::new(t)))
    }
}

impl TcpListener {
    /// Bind to a port
    pub fn bind(addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::tcp::TcpListener::bind(addr).map(|t| TcpListener(RcEvented::new(t)))
    }

    /// Block on accepting a connection.
    pub fn accept(&self) -> io::Result<TcpStream> {
        loop {
            let res = self.shared().try_accept();

            match res {
                Ok(None) => {
                    self.block_on(RW::read())
                },
                Ok(Some(r))  => {
                    return Ok(
                        TcpStream(RcEvented::new(r))
                        );
                },
                Err(e) => {
                    return Err(e)
                }
            }
        }

    }

    /// Attempt to accept a pending connection.
    ///
    /// This will not block.
    pub fn try_accept(&self) -> io::Result<Option<TcpStream>> {
        self.shared().try_accept()
            .map(|t| t.map(|t| TcpStream(RcEvented::new(t))))

    }
}

impl TcpStream {
    /// Local address of connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.shared().0.borrow().io.local_addr()
    }

    /// Shutdown the connection.
    pub fn shutdown(&self, how : mio_orig::tcp::Shutdown) -> io::Result<()> {
        self.shared().0.borrow().io.shutdown(how)
    }
}

impl io::Read for TcpStream {
    /// Block on read.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let res = self.shared().try_read(buf);

            match res {
                Ok(None) => {
                    self.block_on(RW::read())
                },
                Ok(Some(r))  => {
                    return Ok(r);
                },
                Err(e) => {
                    return Err(e)
                }
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
                Ok(None) => {
                    self.block_on(RW::write())
                },
                Ok(Some(r))  => {
                    return Ok(r);
                },
                Err(e) => {
                    return Err(e)
                }
            }
        }
    }

    // TODO: ?
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TcpStream {
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


unsafe impl Send for TcpStream { }
unsafe impl Send for TcpListener { }
