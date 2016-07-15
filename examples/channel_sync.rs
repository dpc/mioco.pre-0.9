#[macro_use]
extern crate mioco;
extern crate env_logger;

use std::net::SocketAddr;
use std::str::FromStr;
use mioco::tcp::TcpListener;
use mioco::Mioco;
use mioco::sync::mpsc::Receiver;
use std::thread;
use std::io::{self, Read, Write, BufRead};

const DEFAULT_LISTEN_ADDR : &'static str = "127.0.0.1:5555";

fn main() {
    env_logger::init().unwrap();
    let (mail_send, mail_recv) = mioco::sync::mpsc::sync_channel::<i32>(5);  

    thread::spawn(move|| {
        loop {
            let stdin = io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).unwrap();
            let _ = mail_send.send(0);
        }
    });

    mioco::start(move || {
                loop {
                    let mut timer = mioco::timer::Timer::new();
                    timer.set_timeout(5000);
                    select!(
                        r:timer => {
                            println!("TIMEELL");
                        },
                        r:mail_recv => {
                            let message = mail_recv.recv();
                            println!("{:?}", message); 
                        },
                    );
                }
    }).unwrap();
}
