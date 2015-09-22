extern crate mio;
extern crate mioco;
extern crate env_logger;

use std::net::{SocketAddr, SocketAddrV4};
use mio::buf::{MutSliceBuf, SliceBuf};
use mio::udp::{UdpSocket};
use mio::Ipv4Addr;

const START_PORT : u16 = 60000;
const END_PORT   : u16 = 65535;

fn main() {
    env_logger::init().unwrap();

    mioco::start(move |mioco| {
        println!("Starting udp echo server on ports: {}-{}", START_PORT, END_PORT);

        for port in START_PORT..END_PORT {
            mioco.spawn(move |mioco| {
                let ip = Ipv4Addr::new(0, 0, 0, 0);
                let addr = SocketAddr::V4(SocketAddrV4::new(ip, port));

                let sock = try!(UdpSocket::v4());
                try!(sock.bind(&addr));
                let mut sock = mioco.wrap(sock);

                let mut buf = [0u8; 1024 * 16];
                loop {
                    let addr = try!(sock.read(&mut MutSliceBuf::wrap(&mut buf)));
                    try!(sock.write(&mut SliceBuf::wrap(&buf), &addr));
                }
            });
        }
        Ok(())
    });
}
