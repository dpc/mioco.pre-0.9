extern crate mioco;
extern crate env_logger;
extern crate httparse;

use std::net::SocketAddr;
use std::str::FromStr;
use std::io::{Write, Read};
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

const RESPONSE_404: &'static str = "HTTP/1.1 404 Not Found\r
Content-Length: 14\r
\r
Hello World\r
\r";


fn main() {
    env_logger::init().unwrap();
    let addr = listend_addr();

    let listener = TcpListener::bind(&addr).unwrap();

    println!("Starting mioco http server on {:?}", listener.local_addr().unwrap());

    mioco::start(move || {
        for _ in 0..mioco::thread_num() {
            let listener = try!(listener.try_clone());
            mioco::spawn(move || {
                loop {
                    let mut conn = try!(listener.accept());
                    mioco::spawn(move || {
                        let mut buf_i = 0;
                        let mut buf = [0u8; 1024];

                        let mut headers = [httparse::EMPTY_HEADER; 16];
                        loop {
                            let len = try!(conn.read(&mut buf[buf_i..]));

                            if len == 0 {
                                return Ok(());
                            }

                            buf_i += len;

                            let mut req = httparse::Request::new(&mut headers);
                            let res = req.parse(&buf[0..buf_i]).unwrap();

                            if res.is_complete() {
                                let req_len = res.unwrap();
                                match req.path {
                                    Some(ref _path) => {
                                        let _ = try!(conn.write_all(&RESPONSE.as_bytes()));
                                        if req_len != buf_i {
                                            // request has a body; TODO: handle it
                                        }
                                        buf_i = 0;
                                    },
                                    None => {
                                        let _ = try!(conn.write_all(&RESPONSE_404.as_bytes()));
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    });
                }
            });
        }
        Ok(())
    });
}
