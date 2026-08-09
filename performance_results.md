# Performance Test Results

**Date:** 2026-08-08 23:03:58

| Test Name                                                      | Status  | Measurement                                                                                                    |
|----------------------------------------------------------------|---------|----------------------------------------------------------------------------------------------------------------|
| Performance: Write first 2500 records                          | Success | 0.25s total; n=2500, p50 0.08ms, p95 0.12ms, p99 0.15ms, max 7.47ms, 10062 ops/s; budget p95 <= 10.00ms        |
| Performance: Write remaining 7500 records                      | Success | 1.00s total; n=7500, p50 0.08ms, p95 0.12ms, p99 0.15ms, max 23.63ms, 7533 ops/s; budget p95 <= 10.00ms        |
| Performance: Write cost stays flat as the file grows           | Success | p50 0.08ms -> 0.08ms while the file grew 4x; 1.01x (limit 2.00x)                                               |
| Performance: Random point reads (1000)                         | Success | all records found; n=1000, p50 0.08ms, p95 0.10ms, p99 0.14ms, max 0.17ms, 12161 ops/s; budget p95 <= 25.00ms  |
| Performance: Unique-match query                                | Success | 1 result(s); n=20, p50 4.30ms, p95 4.37ms, p99 4.50ms, max 4.50ms, 232 ops/s; budget p95 <= 60.00ms            |
| Performance: Attribute query (10% of the file)                 | Success | 1000 result(s); n=20, p50 12.90ms, p95 13.60ms, p99 13.92ms, max 13.92ms, 77 ops/s; budget p95 <= 120.00ms     |
| Performance: Compound query (1% of the file)                   | Success | 100 result(s); n=20, p50 5.57ms, p95 6.05ms, p99 6.36ms, max 6.36ms, 179 ops/s; budget p95 <= 80.00ms          |
| Performance: Full scan                                         | Success | 10000 result(s); n=5, p50 92.50ms, p95 96.23ms, p99 96.23ms, max 96.23ms, 11 ops/s; budget p95 <= 500.00ms     |
| Performance: Full scan cost grows no worse than linearly       | Success | 2500 -> 10000 records, p50 22.30ms -> 92.50ms; 4.15x (limit 7.20x)                                             |
| Performance: SELECT into a named list                          | Success | 1000 key(s); n=20, p50 5.65ms, p95 5.81ms, p99 5.81ms, max 5.81ms, 177 ops/s; budget p95 <= 120.00ms           |
| Performance: GET.NEXT, 2 batches of 500                        | Success | 1000 records drained; n=2, p50 4.25ms, p95 4.95ms, p99 4.95ms, max 4.95ms, 217 ops/s; budget p95 <= 60.00ms    |
| Performance: A write rewrites a small fraction of the file     | Success | 1024 groups, 9.8 records each; largest group 661B of 386780B (0.17%)                                           |
| Performance: Resident memory per record                        | Success | 3206 B/record over 10000 records (budget 8192); peak RSS 31.7MB, final RSS 24.5MB, CPU 2.38s over 33 samples   |
| Performance: CPU time is accounted for                         | Success | 2.38s CPU over 3.24s wall (73% of one core)                                                                    |
| Concurrency: Mutual-TLS connection setup (20 handshakes)       | Success | n=20, p50 2.28ms, p95 2.70ms, p99 5.70ms, max 5.70ms, 407 ops/s; budget p95 <= 150.00ms                        |
| Concurrency: Single-client reads (200 ops)                     | Success | baseline for the scaling checks; n=200, p50 0.10ms, p95 0.14ms, p99 0.31ms, max 0.36ms, 9241 ops/s             |
| Concurrency: 8 concurrent clients, 200 reads each              | Success | 19859 ops/s aggregate over 0.08s; n=1600, p50 0.29ms, p95 0.82ms, p99 1.33ms, max 2.75ms, 2694 ops/s           |
| Concurrency: Throughput does not collapse under contention     | Success | 8 clients reach 2.15x the single-client throughput (19859 vs 9241 ops/s, minimum 0.50x)                        |
| Concurrency: Tail latency degrades no worse than fair queueing | Success | p99 1.33ms under 8 clients vs p95 0.14ms alone; 9.67x (limit 24.00x)                                           |
| Concurrency: 8 concurrent writers, 20 writes each              | Success | 11619 ops/s aggregate; n=160, p50 0.21ms, p95 1.19ms, p99 3.21ms, max 3.44ms, 2507 ops/s                       |
| Concurrency: No writes are lost under contention               | Success | as expected                                                                                                    |
| Concurrency: Connections are released without leaking memory   | Success | 0KB above the final RSS at peak for 8 connections; peak RSS 11.8MB, final RSS 11.8MB, CPU 0.21s over 4 samples |
