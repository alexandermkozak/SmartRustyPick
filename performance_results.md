# Performance Test Results

**Date:** 2026-08-08 22:16:33

| Test Name                                                        | Status  | Performance Data           |
|------------------------------------------------------------------|---------|----------------------------|
| Performance: Write 2000 records                                  | Success | 2.25s total, 1.12ms/record |
| Performance: Unique-match query (WITH SEQ = 1000)                | Success | 1.02ms, 1 result(s)        |
| Performance: Attribute query (WITH VAL1 = Val5)                  | Success | 3.29ms, 200 result(s)      |
| Performance: Compound query (WITH VAL1 = Val5 AND VAL2 = Data55) | Success | 1.30ms, 20 result(s)       |
| Performance: Full scan                                           | Success | 18.38ms, 2000 result(s)    |
| Performance: SELECT into a named list                            | Success | 1.24ms, 200 key(s)         |
| Performance: GET.NEXT batch of 200                               | Success | 1.80ms, 200 record(s)      |
