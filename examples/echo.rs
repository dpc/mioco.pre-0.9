extern crate mioco;
extern crate env_logger;

use std::net::SocketAddr;
use std::str::FromStr;
use std::io::{self, Read, Write};
use mioco::tcp::TcpListener;

const DEFAULT_LISTEN_ADDR : &'static str = "127.0.0.1:5555";

fn listend_addr() -> SocketAddr {
    FromStr::from_str(DEFAULT_LISTEN_ADDR).unwrap()
}

fn main() {
    env_logger::init().unwrap();

    mioco::start(|| -> io::Result<()> {
        let addr = listend_addr();

        let listener = try!(TcpListener::bind(&addr));

        println!("Starting tcp echo server on {:?}", try!(listener.local_addr()));

        loop {
            let mut conn = try!(listener.accept());

            mioco::spawn(move || -> io::Result<()> {
                let mut buf = [0u8; 1024 * 16];
                loop {
                    let size = try!(conn.read(&mut buf));
                    if size == 0 {/* eof */ break; }
                    let _ = try!(conn.write_all(&mut buf[0..size]));
                }

                Ok(())
            });
        }
    }).unwrap().unwrap();
}
