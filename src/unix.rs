use super::RW;
use super::evented::{RcEventSource, Evented, EventedImpl, MioAdapter};
use std::io;
use super::mio_orig;
use std::path::Path;
use std::os::unix::io::RawFd;

/// Unix pipe reader
pub type PipeReader = MioAdapter<mio_orig::unix::PipeReader>;

/// Unix pipe writer
pub type PipeWriter = MioAdapter<mio_orig::unix::PipeWriter>;

/// Unix listener
pub struct UnixListener(RcEventSource<mio_orig::unix::UnixListener>);

impl EventedImpl for UnixListener {
    type Raw = mio_orig::unix::UnixListener;

    fn shared(&self) -> &RcEventSource<Self::Raw> {
        &self.0
    }
}


impl UnixListener {
    /// Bind to a port
    pub fn bind<P: AsRef<Path> + ?Sized>(addr: &P) -> io::Result<Self> {
        mio_orig::unix::UnixListener::bind(addr).map(|t| UnixListener(RcEventSource::new(t)))
    }

    /// Try cloning the socket descriptor.
    pub fn try_clone(&self) -> io::Result<Self> {
        self.shared().io_ref().try_clone().map(|t| UnixListener(RcEventSource::new(t)))
    }
}

/// Unix socket
pub type UnixSocket = MioAdapter<mio_orig::unix::UnixSocket>;

impl UnixSocket {
    /// Returns a new, unbound, Unix domain socket
    pub fn stream() -> io::Result<UnixSocket> {
        mio_orig::unix::UnixSocket::stream().map(|t| MioAdapter::new(t))
    }

    /// Connect the socket to the specified address
    pub fn connect<P: AsRef<Path> + ?Sized>(self, addr: &P) -> io::Result<(UnixStream, bool)> {
        self.shared()
            .io_ref()
            .try_clone()
            .and_then(|t| mio_orig::unix::UnixSocket::connect(t, addr))
            .map(|(t, b)| (MioAdapter::new(t), b))
    }

    /// Bind the socket to the specified address
    pub fn bind<P: AsRef<Path> + ?Sized>(&self, addr: &P) -> io::Result<()> {
        self.shared().io_ref().bind(addr)
    }

    /// Clone
    pub fn try_clone(&self) -> io::Result<Self> {
        self.shared().io_ref().try_clone().map(|t| MioAdapter::new(t))
    }
}

/// Unix stream
pub type UnixStream = MioAdapter<mio_orig::unix::UnixStream>;


impl UnixStream {
    /// Connect UnixStream to `path`
    pub fn connect<P: AsRef<Path> + ?Sized>(path: &P) -> io::Result<UnixStream> {
        mio_orig::unix::UnixStream::connect(path).map(|t| MioAdapter::new(t))
    }

    /// Clone
    pub fn try_clone(&self) -> io::Result<Self> {
        self.shared().io_ref().try_clone().map(|t| MioAdapter::new(t))
    }

    /// Try reading data into a buffer.
    ///
    /// This will not block.
    pub fn try_read_recv_fd(&mut self,
                            buf: &mut [u8])
                            -> io::Result<Option<(usize, Option<RawFd>)>> {
        self.shared().io_mut().try_read_recv_fd(buf)
    }

    /// Block on read.
    pub fn read_recv_fd(&mut self, buf: &mut [u8]) -> io::Result<(usize, Option<RawFd>)> {
        loop {
            let res = self.try_read_recv_fd(buf);

            match res {
                Ok(None) => self.block_on(RW::read()),
                Ok(Some(r)) => {
                    return Ok(r);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Try writing a data from the buffer.
    ///
    /// This will not block.
    pub fn try_write_send_fd(&self, buf: &[u8], fd: RawFd) -> io::Result<Option<usize>> {
        self.shared().io_mut().try_write_send_fd(buf, fd)
    }

    /// Block on write
    pub fn write_send_fd(&mut self, buf: &[u8], fd: RawFd) -> io::Result<usize> {
        loop {
            let res = self.try_write_send_fd(buf, fd);

            match res {
                Ok(None) => self.block_on(RW::write()),
                Ok(Some(r)) => {
                    return Ok(r);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Create a pair of unix pipe (reader and writer)
pub fn pipe() -> io::Result<(PipeReader, PipeWriter)> {
    let (raw_reader, raw_writer) = try!(mio_orig::unix::pipe());

    Ok((MioAdapter::new(raw_reader), MioAdapter::new(raw_writer)))

}
