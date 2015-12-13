use super::{RW, RcEvented, Evented};
use super::prv::EventedPrv;
use std::io;
use super::mio_orig;

/// Unix pipe reader
pub struct PipeReader(RcEvented<mio_orig::unix::PipeReader>);

/// Unix pipe writer
pub struct PipeWriter(RcEvented<mio_orig::unix::PipeWriter>);

/// Create a pair of unix pipe (reader and writer)
pub fn pipe() -> io::Result<(PipeReader, PipeWriter)> {
    let (raw_reader, raw_writer) = try!(mio_orig::unix::pipe());

    Ok((
        PipeReader(
            RcEvented::new(raw_reader),
        ),
        PipeWriter(
            RcEvented::new(raw_writer),
        ),
    ))

}

impl EventedPrv for PipeReader {
    type Raw = mio_orig::unix::PipeReader;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl EventedPrv for PipeWriter {
    type Raw = mio_orig::unix::PipeWriter;

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

impl PipeReader {
    /// Try reading data into a buffer.
    ///
    /// This will not block.
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.shared().try_read(buf)
    }
}

impl io::Write for PipeWriter {
    /// Block on read.
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

impl PipeWriter {
    /// Try writing a data from the buffer.
    ///
    /// This will not block.
    pub fn try_write(&self, buf: &[u8]) -> io::Result<Option<usize>> {
        self.shared().try_write(buf)
    }
}

impl Evented for PipeReader {}
impl Evented for PipeWriter {}

unsafe impl Send for PipeWriter { }
unsafe impl Send for PipeReader { }
