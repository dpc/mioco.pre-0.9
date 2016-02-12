use super::{RW, RcEvented, Evented, EventedPrv};
use std::io;
use super::mio_orig;
use std::path::Path;
use std::os::unix::io::{RawFd, FromRawFd, AsRawFd};

/// Unix pipe reader
pub struct PipeReader(RcEvented<mio_orig::unix::PipeReader>);

unsafe impl Send for PipeReader {}

impl EventedPrv for PipeReader {
    type Raw = mio_orig::unix::PipeReader;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl io::Read for PipeReader {
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

impl PipeReader {
    /// Try reading data into a buffer.
    ///
    /// This will not block.
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.shared().try_read(buf)
    }
}

impl FromRawFd for PipeReader {
    unsafe fn from_raw_fd(fd: RawFd) -> PipeReader {
        PipeReader(RcEvented::new(mio_orig::unix::PipeReader::from_raw_fd(fd)))
    }
}

impl AsRawFd for PipeReader {
    fn as_raw_fd(&self) -> RawFd {
        self.shared().0.borrow_mut().io.as_raw_fd()
    }
}


/// Unix pipe writer
pub struct PipeWriter(RcEvented<mio_orig::unix::PipeWriter>);

unsafe impl Send for PipeWriter {}

impl EventedPrv for PipeWriter {
    type Raw = mio_orig::unix::PipeWriter;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl PipeWriter {
    /// Try writing a data from the buffer.
    ///
    /// This will not block.
    pub fn try_write(&self, buf: &[u8]) -> io::Result<Option<usize>> {
        self.shared().try_write(buf)
    }
}

impl io::Write for PipeWriter {
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

impl FromRawFd for PipeWriter {
    unsafe fn from_raw_fd(fd: RawFd) -> PipeWriter {
        PipeWriter(RcEvented::new(mio_orig::unix::PipeWriter::from_raw_fd(fd)))
    }
}

impl AsRawFd for PipeWriter {
    fn as_raw_fd(&self) -> RawFd {
        self.shared().0.borrow_mut().io.as_raw_fd()
    }
}

/// Unix listener
pub struct UnixListener(RcEvented<mio_orig::unix::UnixListener>);

impl EventedPrv for UnixListener {
    type Raw = mio_orig::unix::UnixListener;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

unsafe impl Send for UnixListener {}

impl UnixListener {
    /// Bind to a port
    pub fn bind<P: AsRef<Path> + ?Sized>(addr: &P) -> io::Result<Self> {
        mio_orig::unix::UnixListener::bind(addr).map(|t| UnixListener(RcEvented::new(t)))
    }

    /// Block on accepting a connection.
    pub fn accept(&self) -> io::Result<UnixStream> {
        loop {
            let res = self.shared().try_accept();

            match res {
                Ok(None) => self.block_on(RW::read()),
                Ok(Some(r)) => {
                    return Ok(UnixStream(RcEvented::new(r)));
                }
                Err(e) => return Err(e),
            }
        }

    }

    /// Attempt to accept a pending connection.
    ///
    /// This will not block.
    pub fn try_accept(&self) -> io::Result<Option<UnixStream>> {
        self.shared()
            .try_accept()
            .map(|t| t.map(|t| UnixStream(RcEvented::new(t))))

    }

    /// Try cloning the socket descriptor.
    pub fn try_clone(&self) -> io::Result<Self> {
        self.shared().0.borrow().io.try_clone().map(|t| UnixListener(RcEvented::new(t)))
    }
}

impl FromRawFd for UnixListener {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixListener {
        UnixListener(RcEvented::new(mio_orig::unix::UnixListener::from_raw_fd(fd)))
    }
}

impl AsRawFd for UnixListener {
    fn as_raw_fd(&self) -> RawFd {
        self.shared().0.borrow_mut().io.as_raw_fd()
    }
}


/// Unix socket
pub struct UnixSocket(RcEvented<mio_orig::unix::UnixSocket>);

unsafe impl Send for UnixSocket {}

impl EventedPrv for UnixSocket {
    type Raw = mio_orig::unix::UnixSocket;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl UnixSocket {
    /// Returns a new, unbound, Unix domain socket
    pub fn stream() -> io::Result<UnixSocket> {
        mio_orig::unix::UnixSocket::stream().map(|t| UnixSocket(RcEvented::new(t)))
    }

    /// Connect the socket to the specified address
    pub fn connect<P: AsRef<Path> + ?Sized>(self, addr: &P) -> io::Result<(UnixStream, bool)> {
        self.shared()
            .0
            .borrow()
            .io
            .try_clone()
            .and_then(|t| mio_orig::unix::UnixSocket::connect(t, addr))
            .map(|(t, b)| (UnixStream(RcEvented::new(t)), b))
    }

    /// Bind the socket to the specified address
    pub fn bind<P: AsRef<Path> + ?Sized>(&self, addr: &P) -> io::Result<()> {
        self.shared().0.borrow().io.bind(addr)
    }

    /// Clone
    pub fn try_clone(&self) -> io::Result<Self> {
        self.shared().0.borrow().io.try_clone().map(|t| UnixSocket(RcEvented::new(t)))
    }
}

impl FromRawFd for UnixSocket {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixSocket {
        UnixSocket(RcEvented::new(mio_orig::unix::UnixSocket::from_raw_fd(fd)))
    }
}

impl AsRawFd for UnixSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.shared().0.borrow_mut().io.as_raw_fd()
    }
}

/// Unix stream
pub struct UnixStream(RcEvented<mio_orig::unix::UnixStream>);

unsafe impl Send for UnixStream {}

impl EventedPrv for UnixStream {
    type Raw = mio_orig::unix::UnixStream;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl UnixStream {
    /// Connect UnixStream to `path`
    pub fn connect<P: AsRef<Path> + ?Sized>(path: &P) -> io::Result<UnixStream> {
        mio_orig::unix::UnixStream::connect(path).map(|t| UnixStream(RcEvented::new(t)))
    }

    /// Clone
    pub fn try_clone(&self) -> io::Result<Self> {
        self.shared().0.borrow().io.try_clone().map(|t| UnixStream(RcEvented::new(t)))
    }

    /// Try reading data into a buffer.
    ///
    /// This will not block.
    pub fn try_read_recv_fd(&mut self,
                            buf: &mut [u8])
                            -> io::Result<Option<(usize, Option<RawFd>)>> {
        self.shared().0.borrow_mut().io.try_read_recv_fd(buf)
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
        self.shared().0.borrow_mut().io.try_write_send_fd(buf, fd)
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

    /// Try reading data into a buffer.
    ///
    /// This will not block.
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.shared().try_read(buf)
    }

    /// Try writing a data from the buffer.
    ///
    /// This will not block.
    pub fn try_write(&self, buf: &[u8]) -> io::Result<Option<usize>> {
        self.shared().try_write(buf)
    }
}

impl io::Read for UnixStream {
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

impl FromRawFd for UnixStream {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixStream {
        UnixStream(RcEvented::new(mio_orig::unix::UnixStream::from_raw_fd(fd)))
    }
}

impl AsRawFd for UnixStream {
    fn as_raw_fd(&self) -> RawFd {
        self.shared().0.borrow_mut().io.as_raw_fd()
    }
}

impl io::Write for UnixStream {
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

/// Create a pair of unix pipe (reader and writer)
pub fn pipe() -> io::Result<(PipeReader, PipeWriter)> {
    let (raw_reader, raw_writer) = try!(mio_orig::unix::pipe());

    Ok((PipeReader(RcEvented::new(raw_reader)),
        PipeWriter(RcEvented::new(raw_writer))))

}
