extern crate mio;
extern crate mioco;
extern crate env_logger;

use std::net::SocketAddr;
use std::str::FromStr;
use std::io::{Read, Write};
use mio::tcp::{TcpSocket};

const DEFAULT_LISTEN_ADDR : &'static str = "127.0.0.1:5555";

fn listend_addr() -> SocketAddr {
    FromStr::from_str(DEFAULT_LISTEN_ADDR).unwrap()
}

fn main() {
    env_logger::init().unwrap();

    mioco::start(move |mioco| {
        let addr = listend_addr();

        let sock = try!(TcpSocket::v4());
        sock.bind(&addr).unwrap();
        let sock = sock.listen(1024).unwrap();

        println!("Starting tcp echo server on {:?}", sock.local_addr().unwrap());

        let sock = mioco.wrap(sock);

        loop {
            let conn = try!(sock.accept());

            mioco.spawn(move |mioco| {
                let mut conn = mioco.wrap(conn);

                let mut buf = [0u8; 1024 * 16];
                let five = mioco.timeout(5000);
                loop {
                    let ev = mioco.select_read_from(&[conn.index(), five.index()]);
                    five.reset();
                    if ev.index() == conn.index() {
                        let size = try!(conn.read(&mut buf));
                        if size == 0 {
                            /* eof */
                            break;
                        }
                        try!(conn.write_all(&mut buf[0..size]));
                    } else {
                        conn.with_raw_mut(|conn| {
                            conn.shutdown(mio::tcp::Shutdown::Both).unwrap();
                        });
                    }
                }

                Ok(())
            })
        }
    });
}
