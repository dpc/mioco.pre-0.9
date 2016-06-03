extern crate mioco;
extern crate env_logger;

use std::io::{self, Read, Write};
use mioco::unix::UnixListener;

const DEFAULT_LISTEN_ADDR : &'static str = "/tmp/.unixecho.socket";

fn main() {
    env_logger::init().unwrap();

    mioco::start(|| -> io::Result<()> {
        let listener = try!(UnixListener::bind(DEFAULT_LISTEN_ADDR));

        println!("Starting unix echo server on {:?}", DEFAULT_LISTEN_ADDR);

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
