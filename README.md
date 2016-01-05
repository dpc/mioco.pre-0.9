# mioco

<p align="center">
  <a href="https://travis-ci.org/dpc/mioco">
      <img src="https://img.shields.io/travis/dpc/mioco/master.svg?style=flat-square" alt="Build Status">
  </a>
  <a href="https://crates.io/crates/mioco">
      <img src="http://meritbadge.herokuapp.com/mioco?style=flat-square" alt="crates.io">
  </a>
  <a href="https://gitter.im/dpc/mioco">
      <img src="https://img.shields.io/badge/GITTER-join%20chat-green.svg?style=flat-square" alt="Gitter Chat">
  </a>
  <br>
  <strong><a href="//dpc.github.io/mioco/">Documentation</a></strong>
</p>


## Code snippet

``` rust
fn main() {
    mioco::start(||{
        let addr = listend_addr();

        let listener = TcpListener::bind(&addr).unwrap();

        println!("Starting tcp echo server on {:?}", listener.local_addr().unwrap());

        loop {
            let mut conn = try!(listener.accept());

            mioco::spawn(move || {
                let mut buf = [0u8; 1024 * 16];
                loop {
                    let size = try!(conn.read(&mut buf));
                    if size == 0 {/* eof */ break; }
                    try!(conn.write_all(&mut buf[0..size]))
                }

                Ok(())
            });
        }
    });
}
```

## Introduction

Scalable, coroutine-based, asynchronous IO handling library for Rust programming language.

Using `mioco` you can handle [`mio`][mio]-based IO, using set of synchronous-IO
handling functions. Based on asynchronous [`mio`][mio] events `mioco` will
cooperatively schedule your handlers.

You can think of `mioco` as of *Node.js for Rust* or *[green threads][green threads] on top of [`mio`][mio]*.

`mioco` is a young project, but we think it's already very useful. See
[projects using mioco](https://github.com/dpc/mioco/wiki/Resources#projects-using-mioco). If
you're considering or already using mioco, please drop us a line on [#mioco gitter.im][mioco gitter].

Read [Documentation](//dpc.github.io/mioco/) for details.

If you need help, try asking on [#mioco gitter.im][mioco gitter]. If still no
luck, try [rust user forum][rust user forum].

To report a bug or ask for features use [github issues][issues].

[rust]: http://rust-lang.org
[mio]: //github.com/carllerche/mio
[colerr]: //github.com/dpc/colerr
[mioco gitter]: https://gitter.im/dpc/mioco
[rust user forum]: https://users.rust-lang.org/
[issues]: //github.com/dpc/mioco/issues
[green threads]: https://en.wikipedia.org/wiki/Green_threads

## Building & running

Note: You must be using [nightly Rust][nightly rust] release. If you're using
[multirust][multirust], which is highly recommended, switch with `multirust default
nightly` command.

To start test echo server:

    cargo run --release --example echo

For daily work:

    make all

[nightly rust]: https://doc.rust-lang.org/book/nightly-rust.html
[multirust]: https://github.com/brson/multirust

# Using in your project

In Cargo.toml:

```
[dependencies]
mioco = "*'
```

In your `main.rs`:

```
#[macro_use]
extern crate mioco;
```

# Projects using mioco:

* [colerr][colerr] - colorize stderr;

Send PR or drop a link on gitter.

# Benchmarks

## HTTP Server

Around **7 million requests per second** using trivial code. It can perform.

```
% wrk -t8 -c100 -d10s http://localhost:5555
Running 10s test @ http://localhost:5555
  8 threads and 100 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     1.94s     1.04s    3.19s    62.78%
    Req/Sec   659.38k   523.27k    3.22M    58.59%
  68646374 requests in 10.01s, 3.39GB read
  Socket errors: connect 0, read 0, write 0, timeout 263
Requests/sec: 6855668.81
Transfer/sec:    346.52MB
```

See [`cheating-http-server.rs`](/examples/cheating-http-server.rs).

## TCP Echo Server

**TODO:** SMP support was added. Rerun the tests. `bench2` is around 370k/s now.

Beware: This is very naive comparison! I tried to run it fairly,
but I might have missed something. Also no effort was spent on optimizing
neither `mioco` nor other tested tcp echo implementations.

In thousands requests per second:

|         | `bench1` | `bench2` |
|:--------|---------:|---------:|
| `libev` | 183      | 225      |
| `node`  | 37       | 42       |
| `mio`   | 156      | 190      |
| `mioco` | 157      | 177      |


Server implementation tested:

* `libev` - https://github.com/dpc/benchmark-echo/blob/master/server_libev.c;
   Note: this implementation "cheats", by waiting only for read events, which works
   in this particular scenario.
* `node` - https://github.com/dpc/node-tcp-echo-server;
* `mio` - https://github.com/dpc/mioecho; TODO: this implementation could use some help.
* `mioco` - https://github.com/dpc/mioco/blob/master/examples/echo.rs;

Benchmarks used:

* `bench1` - https://github.com/dpc/benchmark-echo ; `PARAMS='-t64 -c10 -e10000 -fdata.json'`;
* `bench2` - https://gist.github.com/dpc/8cacd3b6fa5273ffdcce ; `GOMAXPROCS=64 ./tcp_bench  -c=128 -t=30 -a=""`;

Machine used:

* i7-3770K CPU @ 3.50GHz, 32GB DDR3 1800Mhz, some basic overclocking, Fedora 21;
