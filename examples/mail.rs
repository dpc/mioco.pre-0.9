#![recursion_limit="128"]
extern crate mioco;
extern crate env_logger;

use std::collections::HashMap;
use std::sync::Arc;

fn do_command(handler: u32, command: String) {
    println!("[COMMAND] Dummy Command: {} for Handler: {} executed", command, handler);
}

fn main() {
    env_logger::init().unwrap();
    mioco::start(move || {
        let mut mailboxes = HashMap::new();

        for handler in 0..4 {
            let (arg_send, arg_recv) = mioco::mail::mailbox::<(String, mioco::mail::MailboxOuterEnd<usize>)>();
            mailboxes.insert(handler, arg_send);

            mioco::spawn(move || {
                let arg_recv = arg_recv;
                loop {
                    match arg_recv.read() {
                        (command, result_send) => {
                            println!("[Handler Coroutine] Got command");
                            do_command(handler, command);
                            println!("[Handler Coroutine] Command exited");
                            result_send.send(1);
                            println!("[Handler Coroutine] Send back result");
                        },
                    }
                }
            });
        }

        let mailboxes = Arc::new(mailboxes);

        loop {
            //normally accept tcp connections
            //simulate using sleep
            mioco::sleep(2000);

            //spawn a new coroutine for a new "connection"
            let mailbox_ref = mailboxes.clone();
            mioco::spawn(move || {

                //normally read command and provider from tcp here
                let provider = 0;
                let command = "test".to_string();

                let (result_send, result_recv) = mioco::mail::mailbox::<usize>();

                match mailbox_ref.get(&provider) {
                    Some(arg_send) => {

                        println!("[Connection Task] Sending command");
                        arg_send.send((command, result_send));
                        println!("[Connection Task] Waiting for response");
                        result_recv.read();
                        println!("[Connection Task] Command returned");


                    },
                    None => println!("[Connection Task] Provider {} not found for issued command", provider),
                };

                Ok(())
            });
        }
    })
}
