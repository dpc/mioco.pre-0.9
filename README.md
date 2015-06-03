[![Build Status](https://travis-ci.org/dpc/mioco.svg?branch=master)](https://travis-ci.org/dpc/mioco)

# mioco

## Introduction

`mioco` is [rust language][rust] library that allows handling [mio][mio]
connections inside coroutines based on [coroutine][coroutine]

[rust]: http://rust-lang.org
[mio]: https://github.com/carllerche/mio
[coroutine]: https://github.com/rustcc/coroutine-rs

Read [Documentation](//dpc.github.io/mioco/) for details.

## Building & running

    cargo run --release
    make echo

# Semi-benchmarks

`mioco` comes with tcp echo server example, that is being benchmarked here.

Beware: This is amateurish and probably misleading comparison!

Using: https://gist.github.com/dpc/8cacd3b6fa5273ffdcce

```
# mioco echo example
% GOMAXPROCS=64 ./tcp_bench  -c=128 -t=10  -a="127.0.0.1:5555"
Benchmarking: 127.0.0.1:5555
128 clients, running 26 bytes, 10 sec.

Speed: 171022 request/sec, 171022 response/sec
Requests: 1710222
Responses: 1710221

# server_libev (simple libev based server):
% GOMAXPROCS=64 ./tcp_bench  -c=128 -t=10  -a="127.0.0.1:5000"
Benchmarking: 127.0.0.1:3100
128 clients, running 26 bytes, 10 sec.

Speed: 210485 request/sec, 210485 response/sec
Requests: 2104856
Responses: 2104854
```

Using: https://github.com/dpc/benchmark-echo

```
# rust mioco echo example :5555
Throughput: 148697.85 [reqests/sec], errors: 0
Throughput: 145282.09 [reqests/sec], errors: 0
Throughput: 157900.81 [reqests/sec], errors: 0
Throughput: 155722.21 [reqests/sec], errors: 0
Throughput: 160203.92 [reqests/sec], errors: 0

c ./server_libev 3100
Throughput: 192770.52 [reqests/sec], errors: 0
Throughput: 156105.10 [reqests/sec], errors: 0
Throughput: 162632.05 [reqests/sec], errors: 0
Throughput: 179868.05 [reqests/sec], errors: 0
Throughput: 187706.06 [reqests/sec], errors: 0
```
