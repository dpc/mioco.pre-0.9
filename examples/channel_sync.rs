#[macro_use]
extern crate mioco;
extern crate env_logger;

use std::thread;
use std::io::{self, BufRead};
use std::time::Duration;

fn main() {
    env_logger::init().unwrap();
    let (tx1, rx1) = mioco::sync::mpsc::sync_channel::<String>(5);
    let (tx2, rx2) = mioco::sync::mpsc::sync_channel::<String>(5);

    thread::spawn(move|| {
        loop {
            println!("Enter something:");
            let stdin = io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).ok().expect("Failed to read line");
            let line = line.parse::<String>().expect("Not a number");
            let _ = tx1.send(line.clone());
            let _ = tx2.send(line);
        }
    });

    mioco::start(move || {
                loop {
                    let mut timer = mioco::timer::Timer::new();
                    timer.set_timeout(5000);
                    select!(
                        r:timer => {
                            println!("TIMEOUT");
                        },
                        r:rx1 => {
                            println!("1. Reading ...");
                            thread::sleep(Duration::new(2, 0));
                            let message = rx1.recv();
                            println!("{:?}", message); 
                        },
                        r:rx2 => {
                            println!("2. Reading ...");
                            thread::sleep(Duration::new(2, 0));
                            let message = rx2.recv();
                            println!("{:?}", message); 
                        },
                    );
                }
    }).unwrap();
}
