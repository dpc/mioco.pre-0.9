## Mioco Benchmarks

All benchmarks are run on:

* i7-3770K CPU @ 3.50GHz, 32GB DDR3 1800Mhz, some basic overclocking, Fedora 21;

### HTTP Server

This test use mioco instance with 8 threads (on 8 CPU cores).

```
% wrk -t8 -c100 -d10s http://localhost:5555
Running 10s test @ http://localhost:5555
  8 threads and 100 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     3.55s     1.13s    4.29s    91.22%
    Req/Sec   477.29k   695.20k    3.25M    83.20%
  100928033 requests in 10.39s, 4.98GB read
  Socket errors: connect 0, read 0, write 0, timeout 100
Requests/sec: 9710448.56
Transfer/sec:    490.81MB
```

Around **10 million requests per second** using trivial code. This test
does not really fully parse and handle HTTP, but shows that event handling itself
is fast.

See [`cheating-http-server.rs`](/examples/cheating-http-server.rs).

With proper HTTP handling using `httparse` lib:

```
% wrk -t8 -c100 -d10s http://localhost:5555
Running 10s test @ http://localhost:5555
  8 threads and 100 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency   305.80us    0.97ms  25.04ms   94.54%
    Req/Sec    49.48k    18.39k  113.00k    67.86%
  3684835 requests in 10.00s, 186.25MB read
Requests/sec: 368543.39
Transfer/sec:     18.63MB
```

## TCP Echo Server

Beware: This is very naive comparison! I tried to run it fairly,
but I might have missed something.

In thousands requests per second:

|                    | `bench1` | `bench2` |
|:-------------------|---------:|---------:|
| `libev`            | 158      | 167      |
| `node`             |  40      |  40      |
| `mio`              | 133      | 148      |
| `mioco` (1 thread) | 116      | 127      |
| `mioco` (8 threads)| 341      | 333      |


Server implementation tested:

* `libev` - https://github.com/dpc/benchmark-echo/blob/master/server_libev.c ;
   Note: this implementation "cheats", by waiting only for read events, which works
   in this particular scenario.
* `node` - https://github.com/dpc/node-tcp-echo-server ;
* `mio` - https://github.com/dpc/mioecho; TODO: this implementation could use some help.
* `mioco` - https://github.com/dpc/mioco/blob/master/examples/echo.rs ;

Benchmarks used:

* `bench1` - https://github.com/dpc/benchmark-echo ; `PARAMS='-t64 -c10 -e10000 -fdata.json'`;
* `bench2` - https://gist.github.com/dpc/8cacd3b6fa5273ffdcce ; `GOMAXPROCS=64 ./tcp_bench  -c=128 -t=30 -a=""`;
