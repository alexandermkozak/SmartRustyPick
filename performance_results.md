# Performance Test Results

**Date:** 2026-08-08 22:30:57

| Test Name                                                      | Status  | Measurement                                                                                                    |
|----------------------------------------------------------------|---------|----------------------------------------------------------------------------------------------------------------|
| Performance: Write first 500 records                           | Success | 0.19s total; n=500, p50 0.37ms, p95 0.59ms, p99 0.76ms, max 0.88ms, 2677 ops/s; budget p95 <= 32.00ms          |
| Performance: Write remaining 1500 records                      | Success | 2.09s total; n=1500, p50 1.37ms, p95 2.11ms, p99 2.20ms, max 2.69ms, 719 ops/s; budget p95 <= 32.00ms          |
| Performance: Write cost grows no worse than the file size      | Success | p50 0.37ms -> 1.37ms at 5.0x the average file size; 3.67x (limit 7.50x)                                        |
| Performance: Random point reads (1000)                         | Success | all records found; n=1000, p50 0.08ms, p95 0.10ms, p99 0.11ms, max 0.16ms, 11766 ops/s; budget p95 <= 100.00ms |
| Performance: Unique-match query                                | Success | 1 result(s); n=20, p50 0.94ms, p95 0.97ms, p99 1.00ms, max 1.00ms, 1064 ops/s; budget p95 <= 48.00ms           |
| Performance: Attribute query (10% of the file)                 | Success | 200 result(s); n=20, p50 2.66ms, p95 2.69ms, p99 2.69ms, max 2.69ms, 375 ops/s; budget p95 <= 96.00ms          |
| Performance: Compound query (1% of the file)                   | Success | 20 result(s); n=20, p50 1.25ms, p95 1.27ms, p99 1.31ms, max 1.31ms, 798 ops/s; budget p95 <= 64.00ms           |
| Performance: Full scan                                         | Success | 2000 result(s); n=5, p50 17.30ms, p95 19.14ms, p99 19.14ms, max 19.14ms, 57 ops/s; budget p95 <= 400.00ms      |
| Performance: Full scan cost grows no worse than linearly       | Success | 500 -> 2000 records, p50 4.34ms -> 17.30ms; 3.99x (limit 7.20x)                                                |
| Performance: SELECT into a named list                          | Success | 200 key(s); n=20, p50 1.14ms, p95 1.19ms, p99 1.50ms, max 1.50ms, 861 ops/s; budget p95 <= 96.00ms             |
| Performance: GET.NEXT, 1 batches of 200                        | Success | 200 records drained; n=1, p50 1.81ms, p95 1.81ms, p99 1.81ms, max 1.81ms, 554 ops/s; budget p95 <= 240.00ms    |
| Performance: Resident memory per record                        | Success | 6674 B/record over 2000 records (budget 8192); peak RSS 13.6MB, final RSS 12.3MB, CPU 2.49s over 28 samples    |
| Performance: CPU time is accounted for                         | Success | 2.49s CPU over 2.73s wall (91% of one core)                                                                    |
| Concurrency: Mutual-TLS connection setup (20 handshakes)       | Success | n=20, p50 2.24ms, p95 2.50ms, p99 2.59ms, max 2.59ms, 439 ops/s; budget p95 <= 600.00ms                        |
| Concurrency: Single-client reads (200 ops)                     | Success | baseline for the scaling checks; n=200, p50 0.09ms, p95 0.11ms, p99 0.13ms, max 0.35ms, 10874 ops/s            |
| Concurrency: 4 concurrent clients, 200 reads each              | Success | 26810 ops/s aggregate over 0.03s; n=800, p50 0.13ms, p95 0.17ms, p99 0.22ms, max 0.49ms, 7657 ops/s            |
| Concurrency: Throughput does not collapse under contention     | Success | 4 clients reach 2.47x the single-client throughput (26810 vs 10874 ops/s, minimum 0.50x)                       |
| Concurrency: Tail latency degrades no worse than fair queueing | Success | p99 0.22ms under 4 clients vs p95 0.11ms alone; 1.89x (limit 12.00x)                                           |
| Concurrency: 4 concurrent writers, 20 writes each              | Success | 932 ops/s aggregate; n=80, p50 3.00ms, p95 3.64ms, p99 4.26ms, max 60.62ms, 307 ops/s                          |
| Concurrency: No writes are lost under contention               | Success | as expected                                                                                                    |
| Concurrency: Connections are released without leaking memory   | Success | 0KB above the final RSS at peak for 4 connections; peak RSS 11.7MB, final RSS 11.7MB, CPU 0.67s over 9 samples |
