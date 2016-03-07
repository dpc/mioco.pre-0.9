extern crate mioco;
extern crate env_logger;

use std::net::SocketAddr;
use std::str::FromStr;
use std::io::{Read, Write};
use mioco::tcp::TcpListener;

const DEFAULT_LISTEN_ADDR : &'static str = "127.0.0.1:5555";

fn listend_addr() -> SocketAddr {
    FromStr::from_str(DEFAULT_LISTEN_ADDR).unwrap()
}

fn main() {
    env_logger::init().unwrap();

    mioco::start(||{
        let addr = listend_addr();

        let listener = TcpListener::bind(&addr).unwrap();

        println!("Starting tcp echo server on {:?}", listener.local_addr().unwrap());

        loop {
            let mut conn = listener.accept().unwrap();

            mioco::spawn(move || {
                let mut buf = [0u8; 1024 * 16];
                loop {
                    let size = conn.read(&mut buf).unwrap();
                    if size == 0 {/* eof */ break; }
                    conn.write_all(&mut buf[0..size]).unwrap();
                }
            });
        }
    }).unwrap();
}
