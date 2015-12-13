use super::{RW, RcEvented, Evented};
use super::prv::EventedPrv;
use super::mio_orig;
use std::io;
use std::net::SocketAddr;

/// Udp Socket
pub struct UdpSocket(RcEvented<mio_orig::udp::UdpSocket>);

impl EventedPrv for UdpSocket {
    type Raw = mio_orig::udp::UdpSocket;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl Evented for UdpSocket { }

impl UdpSocket {
    /// Return a new unbound IPv4 UDP Socket.
    pub fn v4() -> io::Result<Self> {
        mio_orig::udp::UdpSocket::v4().map(|t| UdpSocket(RcEvented::new(t)))
    }

    /// Return a new unbound IPv6 UDP Socket.
    pub fn v6() -> io::Result<Self> {
        mio_orig::udp::UdpSocket::v6().map(|t| UdpSocket(RcEvented::new(t)))
    }

    /// Return a new bound UDP Socket.
    pub fn bound(addr: &SocketAddr) -> io::Result<Self> {
        mio_orig::udp::UdpSocket::bound(addr).map(|t| UdpSocket(RcEvented::new(t)))
    }

    /// Bind the unbound UDP Socket.
    pub fn bind(&self, addr: &SocketAddr) -> io::Result<()> {
        self.shared().0.borrow().io.bind(addr)

    }

    /// Local address of the Socket.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.shared().0.borrow().io.local_addr()
    }

    /// Try cloning the socket.
    pub fn try_clone(&self) -> io::Result<UdpSocket> {
        self.shared().0.borrow().io.try_clone().map(|t| UdpSocket(RcEvented::new(t)))
    }

    /// Block on read.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        loop {
            let res = self.try_read(buf);

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

    /// Try reading data into a buffer.
    ///
    /// This will not block.
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        self.shared().0.borrow().io.recv_from(buf)
    }

    /// Block on write.
    pub fn write(&mut self, buf: &[u8], target : &SocketAddr) -> io::Result<usize> {
        loop {
            let res = self.try_write(buf, target);

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

    /// Try writing a data from the buffer.
    ///
    /// This will not block.
    pub fn try_write(&self, buf: &[u8], target : &SocketAddr) -> io::Result<Option<usize>> {
        self.shared().0.borrow().io.send_to(buf, target)
    }
}

unsafe impl Send for UdpSocket { }
