extern crate mioco;
extern crate env_logger;

use std::net::{SocketAddr, SocketAddrV4};
use std::io;
use mioco::udp::{UdpSocket};
use mioco::mio::Ipv4Addr;

const START_PORT : u16 = 60000;
const END_PORT   : u16 = 65535;

fn main() {
    env_logger::init().unwrap();

    mioco::start(move || {
        println!("Starting udp echo server on ports: {}-{}", START_PORT, END_PORT);

        for port in START_PORT..END_PORT {
            mioco::spawn(move || -> io::Result<()> {
                let ip = Ipv4Addr::new(0, 0, 0, 0);
                let addr = SocketAddr::V4(SocketAddrV4::new(ip, port));

                let mut sock = UdpSocket::v4().unwrap();
                sock.bind(&addr).unwrap();
                let mut buf = [0u8; 1024 * 16];
                loop {
                    let (len, addr) = try!(sock.recv(&mut buf));
                    try!(sock.send(&mut buf[0..len], &addr));
                }
            });
        }
    }).unwrap();
}
