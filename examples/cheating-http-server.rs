extern crate mioco;
extern crate env_logger;

use std::net::SocketAddr;
use std::str::FromStr;
use std::io::Write;
use mioco::tcp::TcpListener;

const DEFAULT_LISTEN_ADDR : &'static str = "127.0.0.1:5555";

fn listend_addr() -> SocketAddr {
    FromStr::from_str(DEFAULT_LISTEN_ADDR).unwrap()
}

const RESPONSE: &'static str = "HTTP/1.1 200 OK\r
Content-Length: 14\r
\r
Hello World\r
\r";

fn main() {
    env_logger::init().unwrap();
    let addr = listend_addr();

    let listener = TcpListener::bind(&addr).unwrap();

    println!("Starting \"cheating\" http server on {:?}", listener.local_addr().unwrap());

    mioco::start(move || {
        for _ in 0..mioco::thread_num() {
            let listener = listener.try_clone().unwrap();
            mioco::spawn(move || {
                loop {
                    let conn = listener.accept().unwrap();
                    mioco::spawn(move || {
                        let mut conn = conn;
                        loop {
                            let _ = conn.write_all(&RESPONSE.as_bytes()).unwrap();
                        }
                    });
                }
            });
        }
    }).unwrap();
}
