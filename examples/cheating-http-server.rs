extern crate mioco;
extern crate env_logger;

use std::net::SocketAddr;
use std::str::FromStr;
use std::io::Write;
use mioco::mio::tcp::{TcpSocket};

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

    let sock = TcpSocket::v4().unwrap();
    sock.bind(&addr).unwrap();
    let sock = sock.listen(1024).unwrap();

    println!("Starting \"cheating\" http server on {:?}", sock.local_addr().unwrap());

    mioco::start(move |mioco| {
        for _ in 0..mioco.thread_num() {
            let sock = try!(sock.try_clone());
            mioco.spawn(move |mioco| {
                let sock = mioco.wrap(sock);
                loop {
                    let conn = try!(sock.accept());
                    mioco.spawn(move |mioco| {
                        let mut conn = mioco.wrap(conn);
                        loop {
                            let _ = try!(conn.write_all(&RESPONSE.as_bytes()));
                        }
                    });
                }
            });
        }
        Ok(())
    });
}
