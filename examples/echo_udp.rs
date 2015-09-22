extern crate mio;
extern crate mioco;
extern crate env_logger;

use std::net::SocketAddr;
use std::str::FromStr;

use mio::buf::{MutSliceBuf, SliceBuf};
use mio::udp::{UdpSocket};
const DEFAULT_LISTEN_ADDR : &'static str = "127.0.0.1:5555";

fn listend_addr() -> SocketAddr {
    FromStr::from_str(DEFAULT_LISTEN_ADDR).unwrap()
}

fn main() {
    env_logger::init().unwrap();

    mioco::start(move |mioco| {
        let addr = listend_addr();

        let sock = try!(UdpSocket::v4());
        try!(sock.bind(&addr));

        println!("Starting udp echo server on {:?}", sock.local_addr().unwrap());

        let mut sock = mioco.wrap(sock);

        let mut buf = [0u8; 1024 * 16];

        loop {
            let addr = try!(sock.read(&mut MutSliceBuf::wrap(&mut buf)));
            try!(sock.write(&mut SliceBuf::wrap(&buf), &addr));
        }
    });
}
