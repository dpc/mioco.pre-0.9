use super::start;

use std;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use mio;

#[test]
fn empty_handler() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |_| {
        let mut lock = finished_copy.lock().unwrap();
        *lock = true;

        Ok(())
    });

    assert!(*finished_ok.lock().unwrap());
}

#[test]
fn empty_subcoroutines() {
    let counter = Arc::new(Mutex::new(0i32));

    let counter_copy = counter.clone();

    start(move |mioco| {

        for _ in 0..512 {
            let counter_subcopy = counter_copy.clone();
            mioco.spawn(move |_| {
                let mut lock = counter_subcopy.lock().unwrap();
                *lock += 1;

                Ok(())
            });
        }

        let mut lock = counter_copy.lock().unwrap();
        *lock += 1;

        Ok(())
    });

    assert_eq!(*counter.lock().unwrap(), 512 + 1);
}

#[test]
fn contain_panics() {
    let finished_ok = Arc::new(Mutex::new(false));

    start(move |_| {
        panic!()
    });

    assert!(!*finished_ok.lock().unwrap());
}


#[test]
fn contain_panics_in_subcoroutines() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |mioco| {

        for _ in 0..512 {
            mioco.spawn(|_| {
                panic!()
            });
        }

        let mut lock = finished_copy.lock().unwrap();
        *lock = true;

        Ok(())
    });

    assert!(*finished_ok.lock().unwrap());
}

#[test]
fn long_chain() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |mioco| {

        let (first_reader, first_writer) = try!(mio::unix::pipe());

        let mut prev_reader = first_reader;

        // TODO: increase after https://github.com/dpc/mioco/issues/8 is fixed
        for _ in 0..128 {
            let (reader, writer) = try!(mio::unix::pipe());

            mioco.spawn(move |mioco| {
                let mut reader = mioco.wrap(prev_reader);
                let mut writer = mioco.wrap(writer);

                let _ = std::io::copy(&mut reader, &mut writer);

                Ok(())
            });

            prev_reader = reader;
        }

        let mut first_writer = mioco.wrap(first_writer);
        let mut last_reader = mioco.wrap(prev_reader);

        for i in 0..9 {
            let test_str = format!("TeSt{}", i);
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
