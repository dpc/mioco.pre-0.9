# mioco

<p align="center">
  <a href="https://travis-ci.org/dpc/mioco">
      <img src="https://img.shields.io/travis/dpc/mioco/master.svg?style=flat-square" alt="Travis CI Build Status">
  </a>
  <a href="https://ci.appveyor.com/project/dpc/mioco/branch/master">
      <img src="https://ci.appveyor.com/api/projects/status/p5rjfbqw2a3pxc4o/branch/master?svg=true" alt="App Veyor Build Status">
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

## Project status

I (dpc, the main author) cosider this project **Abandoned** ATM. The code is still working,
and I'll be happy to merge fixes and so on, but I don't plan to actively develop it further
myself. I would be happy to hand it over if anyone is interested.

Rust community decided that `futures` should be main Rust async IO story, and we should
stick to it.

Right now I'm working on (looking for help, as always!) on
[tokio-fiber: coroutines as `Future`s](https://github.com/dpc/tokio-fiber/)
project, which should have mioco-like API and allow easily porting code using `mioco`.

## Code snippet

``` rust
    mioco::start(|| -> io::Result<()> {
        let addr = listend_addr();

        let listener = try!(TcpListener::bind(&addr));

        println!("Starting tcp echo server on {:?}", try!(listener.local_addr()));

        loop {
            let mut conn = try!(listener.accept());

            mioco::spawn(move || -> io::Result<()> {
                let mut buf = [0u8; 1024 * 16];
                loop {
                    let size = try!(conn.read(&mut buf));
                    if size == 0 {/* eof */ break; }
                    let _ = try!(conn.write_all(&mut buf[0..size]));
                }

                Ok(())
            });
        }
    }).unwrap().unwrap();
```

This trivial code scales very well. See [benchmarks](BENCHMARKS.md).

## Contributors welcome!

Mioco is looking for contributors. See
[Contributing page](https://github.com/dpc/mioco/wiki/Contributing)
for details.

## Introduction

Scalable, coroutine-based, asynchronous IO handling library for Rust
programming language.

Mioco uses asynchronous event loop, to cooperatively switch between
coroutines (aka. green threads), depending on data availability. You
can think of `mioco` as *Node.js for Rust* or Rust *[green
threads][green threads] on top of [`mio`][mio]*.

Read [Documentation](//dpc.github.io/mioco/) for details and features.

If you want to say hi, or need help use [#mioco gitter.im][mioco gitter].

To report a bug or ask for features use [github issues][issues].

[rust]: http://rust-lang.org
[mio]: //github.com/carllerche/mio
[colerr]: //github.com/dpc/colerr
[mioco gitter]: https://gitter.im/dpc/mioco
[rust user forum]: https://users.rust-lang.org/
[issues]: //github.com/dpc/mioco/issues
[green threads]: https://en.wikipedia.org/wiki/Green_threads

## Building & running

### Standalone

To start test echo server:

    cargo run --release --example echo

For daily work:

    make all

[multirust]: https://github.com/brson/multirust

### In your project

In Cargo.toml:

```
[dependencies]
mioco = "*"
```

In your `main.rs`:

```
#[macro_use]
extern crate mioco;
```

## Projects using mioco:

* [colerr][colerr] - colorize stderr;
* [mioco-openssl example](https://github.com/sp3d/mioco-openssl-example) 

Send PR or drop a link on gitter.
