#![feature(convert)]

extern crate mioco;
extern crate env_logger;
#[macro_use]
extern crate log;

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};


fn main() {
    env_logger::init().unwrap();
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    mioco::start(move || {

        let (first_reader, first_writer) = try!(mioco::unix::pipe());

        let mut prev_reader = first_reader;

        // TODO: increase after https://github.com/dpc/mioco/issues/8 is fixed
        for _ in 0..128 {
            let (reader, writer) = try!(mioco::unix::pipe());

            mioco::spawn(move || {
                let mut reader = prev_reader;
                let mut writer = writer;

                let _ = std::io::copy(&mut reader, &mut writer);

                Ok(())
            });

            prev_reader = reader;
        }

        let mut first_writer = first_writer;
        let mut last_reader = prev_reader;

        for i in 0..9 {
            let test_str = format!("TeSt{}", i);
            debug!("Sending \"{}\"", test_str);
            let _ = first_writer.write_all(test_str.as_str().as_bytes());
            let mut buf = [0u8; 16];

            let _ = last_reader.read(&mut buf);

            if &buf[0..5] != test_str.as_str().as_bytes() {
                panic!();
            }
        }

        let mut lock = finished_copy.lock().unwrap();
        *lock = true;

        Ok(())
    });

    assert!(*finished_ok.lock().unwrap());
}
