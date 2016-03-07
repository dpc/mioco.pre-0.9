extern crate mioco;
extern crate env_logger;

/// Example on how to shutdown mioco instance from "the outside".
fn main() {
    env_logger::init().unwrap();

    let (terminate_tx, terminate_rx) = mioco::sync::mpsc::channel();

    println!("Spawning mioco instance");
    mioco::spawn(move || {
        mioco::spawn(move || {
            let _ = terminate_rx.recv();
            mioco::shutdown();
        });

        for _ in 0..32 {
            mioco::spawn(move|| {
                loop {
                    mioco::yield_now();
                }
            });
        }
    });

    println!("Sleeping");
    std::thread::sleep(std::time::Duration::from_secs(1));

    println!("Terminating mioco instance");
    terminate_tx.send(()).unwrap();
}
