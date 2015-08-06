use super::start;

use std::sync::{Arc, Mutex};

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
