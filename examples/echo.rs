#![feature(result_expect)]
#![feature(drain)]
extern crate mio;
extern crate nix;
extern crate coroutine;
extern crate mioco;

use mio::*;
use mio::tcp::*;
use mio::util::Slab;
use std::io;
use std::collections::HashSet;

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
    to_del: HashSet<Token>,
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

        try!(ev_loop.register_opt(&sock, SERVER, Interest::readable(), PollOpt::edge()));

        Ok((Server {
            sock: sock,
            conns: Slab::new_starting_at(CONN_TOKEN_START, CONNS_MAX),
            to_del: HashSet::new(),
        }, ev_loop))
    }

    fn accept(&mut self, event_loop: &mut EventLoop<Server>) -> io::Result<()> {

        use std::os::unix::io::AsRawFd;
        use nix::sys::socket;
        loop {
            let sock = match try!(self.sock.accept()) {
                None => break,
                Some(sock) => sock,
            };

            // Don't buffer output in TCP - kills latency sensitive benchmarks
            try!(socket::setsockopt(
                    sock.as_raw_fd(), socket::SockLevel::Tcp, socket::sockopt::TcpNoDelay, &true
                    ).map_err(|e| io::Error::from_raw_os_error(e.errno() as i32)));

            for token in self.to_del.drain() {
                self.conns.remove(token);
            }

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
                            Err(_) => return,
                            Ok(0) => /* EOF */ return,
                            Ok(size) => {
                                let mut write_i = 0;
                                loop {
                                    if write_i == size {
                                        break;
                                    }

                                    match io.write(&mut buf[write_i..size]) {
                                        Ok(written) => write_i += written,
                                        Err(_) => return,
                                    }
                                }
                            }
                        }
                    }
                };

                builder.start(f);

                io
            });
        }

        Ok(())
    }

    fn conn_handle_finished(&mut self, event_loop : &mut EventLoop<Server>, token : Token, finished : bool) {
        if finished {
            if !self.to_del.contains(&token) {
                let handle = self.conns[token].clone();

                handle.for_every_token(|token| {
                    let mut handle = &mut self.conns[token];
                    handle.deregister(event_loop);
                    self.to_del.insert(token);
                });
            }
        }
    }

    fn conn_readable(&mut self, event_loop: &mut EventLoop<Server>, tok: Token, hint: ReadHint) {
        let finished = {
            let conn = self.conn(tok);
            conn.readable(event_loop, tok, hint);
            conn.is_finished()
        };
        self.conn_handle_finished(event_loop, tok, finished);
    }

    fn conn_writable(&mut self, event_loop: &mut EventLoop<Server>, tok: Token) {
        let finished = {
            let conn = self.conn(tok);
            conn.writable(event_loop, tok);
            conn.is_finished()
        };
        self.conn_handle_finished(event_loop, tok, finished);
    }

    fn conn<'a>(&'a mut self, tok: Token) -> &'a mut mioco::ExternalHandle {
        &mut self.conns[tok]
    }
}

impl Handler for Server {
    type Timeout = usize;
    type Message = ();

    fn readable(&mut self, event_loop: &mut EventLoop<Server>, token: Token, hint: ReadHint) {
        match token {
            SERVER => self.accept(event_loop).expect("accept(event_loop) failed"),
            i => self.conn_readable(event_loop, i, hint),
        };
    }

    fn writable(&mut self, event_loop: &mut EventLoop<Server>, token: Token) {
        match token {
            SERVER => panic!("received writable for token 0"),
            _ => self.conn_writable(event_loop, token),
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
