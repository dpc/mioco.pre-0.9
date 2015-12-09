extern crate mioco;
extern crate env_logger;

use std::net::SocketAddr;
use std::str::FromStr;
use std::io::{Read, Write};
use mioco::mio::tcp::TcpListener;

const DEFAULT_LISTEN_ADDR : &'static str = "127.0.0.1:5555";

fn listend_addr() -> SocketAddr {
    FromStr::from_str(DEFAULT_LISTEN_ADDR).unwrap()
}

fn main() {
    env_logger::init().unwrap();

    mioco::start(move || {
        let addr = listend_addr();

        let listener = TcpListener::bind(&addr).unwrap();

        println!("Starting tcp echo server on {:?}", listener.local_addr().unwrap());

        let listener = mioco::wrap(listener);

        loop {
            let conn = try!(listener.accept());

            mioco::spawn(move || {
                let mut conn = mioco::wrap(conn);

                let mut buf = [0u8; 1024 * 16];
                let timer_id = mioco::timer().id();
                loop {
                    mioco::timer().set_timeout(5000);

                    let ev = mioco::select_read_from(&[conn.id(), timer_id]);
                    if ev.id() == conn.id() {
                        let size = try!(conn.read(&mut buf));
                        if size == 0 {
                            /* eof */
                            break;
                        }
                        try!(conn.write_all(&mut buf[0..size]));
                    } else {
                        conn.with_raw_mut(|conn| {
                            conn.shutdown(mioco::mio::tcp::Shutdown::Both).unwrap();
                        });
                    }
                }

                Ok(())
            });
        }
    });
}
