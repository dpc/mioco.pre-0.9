#![feature(result_expect)]
extern crate mio;
extern crate nix;
extern crate coroutine;
extern crate mioco;

use mio::*;
use mio::tcp::*;
use mio::util::Slab;
use std::io;

use std::net::SocketAddr;
use std::str::FromStr;

const SERVER: Token = Token(0);
const CONN_TOKEN_START : Token = Token(1);
const CONNS_MAX : usize = 256;
const DEFAULT_LISTEN_ADDR : &'static str = "127.0.0.1:5555";

fn listend_addr() -> SocketAddr {
    FromStr::from_str(DEFAULT_LISTEN_ADDR).unwrap()
}

struct Server {
    sock: TcpListener,
    conns: Slab<mioco::ExternalHandle>,
}

impl Server {
    fn new(addr : SocketAddr) -> io::Result<(Server, EventLoop<Server>)> {

        let sock = try!(TcpSocket::v4());

        try!(sock.set_reuseaddr(true));
        try!(sock.bind(&addr));

        let sock = try!(sock.listen(1024));
        let config = EventLoopConfig {
            io_poll_timeout_ms: 1,
            notify_capacity: 4_096,
            messages_per_tick: 256,
            timer_tick_ms: 1,
            timer_wheel_size: 1_024,
            timer_capacity: 65_536,
        };
        let mut ev_loop : EventLoop<Server> = try!(EventLoop::configured(config));

        try!(ev_loop.register_opt(&sock, SERVER, EventSet::readable(), PollOpt::edge()));

        Ok((Server {
            sock: sock,
            conns: Slab::new_starting_at(CONN_TOKEN_START, CONNS_MAX),
        }, ev_loop))
    }

    fn accept(&mut self, event_loop: &mut EventLoop<Server>) -> io::Result<()> {
        loop {
            let sock = match try!(self.sock.accept()) {
                None => break,
                Some(sock) => sock,
            };
            sock.set_nodelay(true).expect("set_nodelay");

            let _tok = self.conns.insert_with(|token| {
                let mut builder = mioco::Builder::new();
                let io : mioco::ExternalHandle = builder.wrap_io(event_loop, sock, token);

                let f = move |io : &mut mioco::CoroutineHandle| {
                    use std::io::{Read, Write};
                    let mut io = &mut io.handles()[0];

                    let mut buf = [0u8; 1024 * 16];
                    loop {
                        let res = io.read(&mut buf);

                        match res {
                            Err(_) => break,
                            Ok(0) => /* EOF */ break,
                            Ok(size) => {
                                if let Err(_) = io.write_all(&mut buf[0..size]) {
                                    break;
                                }
                            }
                        }
                    }

                    io.with_raw_mut(|mut io| {let _ = io.close_outbound();} /* ignore errors */);
                };

                builder.start(f, event_loop);

                io
            });
        }

        Ok(())
    }

    fn conn_ready(&mut self, event_loop : &mut EventLoop<Server>, token: Token, events : EventSet) {
        let finished = {
            let conn = self.conn(token);
            conn.ready(event_loop, token, events);
            conn.is_finished()
        };

        if finished {
            let mut io = self.conns.remove(token).unwrap();
            io.deregister(event_loop);
        }
    }

    fn conn<'a>(&'a mut self, tok : Token) -> &'a mut mioco::ExternalHandle {
        &mut self.conns[tok]
    }
}

impl Handler for Server {
    type Timeout = usize;
    type Message = ();

    fn ready(&mut self, event_loop: &mut EventLoop<Server>, token: Token, events: EventSet) {
        match token {
            SERVER => self.accept(event_loop).expect("accept(event_loop) failed"),
            i => self.conn_ready(event_loop, i, events),
        };
    }
}

pub fn main() {
    let addr = listend_addr();

    let (mut server, mut ev_loop) = Server::new(addr).expect("Server::new(...) failed");

    // Start the event loop
    println!("Starting tcp echo server on {:?}", server.sock.local_addr().unwrap());
    ev_loop.run(&mut server).unwrap();
}
