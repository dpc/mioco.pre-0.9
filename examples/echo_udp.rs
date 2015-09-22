extern crate mio;
extern crate mioco;
extern crate env_logger;

use std::net::SocketAddr;
use std::str::FromStr;

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

        loop {
            let mut buf = mio::buf::ByteBuf::mut_with_capacity(1024 * 16);
            let res = try!(sock.try_read(&mut buf));

            let mut buf = buf.flip();

            match res {
                Some(addr) => {
                    try!(sock.try_write(&mut buf, &addr));
                },
                None => {},
            }
        }
    });
}
