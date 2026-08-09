# Integration Test Results

**Date:** 2026-08-08 22:16:30

| Test Name                                                          | Status  | Details                                                |
|--------------------------------------------------------------------|---------|--------------------------------------------------------|
| Server protocol: WRITE                                             | Success | as expected                                            |
| Server protocol: READ returns the structured record                | Success | as expected                                            |
| Server protocol: QUERY by ID                                       | Success | as expected                                            |
| Server protocol: QUERY by dictionary name                          | Success | as expected                                            |
| Server protocol: SELECT builds a named list                        | Success | as expected                                            |
| Server protocol: GET.NEXT walks the list                           | Success | as expected                                            |
| Server protocol: GET.NEXT reports EOF at the end                   | Success | as expected                                            |
| Server protocol: DELETE                                            | Success | as expected                                            |
| Server protocol: READ after DELETE reports a missing record        | Success | Record not found                                       |
| Server protocol: READ without a file is rejected                   | Success | as expected                                            |
| Headless: CLI seeds the account and file                           | Success | -                                                      |
| Headless: Headless server is reachable and answers the protocol    | Success | as expected                                            |
| Headless: Headless server resolves the seeded account              | Success | as expected                                            |
| Headless: CLI auto-logs in based on the current directory          | Success | -                                                      |
| Headless: Headless server picks up records written by the CLI      | Success | as expected                                            |
| Security: Non-admin CREATE.ACCOUNT is blocked                      | Success | as expected                                            |
| Security: Admin CREATE.ACCOUNT is allowed                          | Success | as expected                                            |
| Security: Non-admin CREATE.FILE is blocked                         | Success | as expected                                            |
| Security: Admin CREATE.FILE is allowed                             | Success | as expected                                            |
| Security: Non-admin AUTHORIZE.CONN is blocked                      | Success | as expected                                            |
| Security: Admin AUTHORIZE.CONN is allowed                          | Success | as expected                                            |
| Security: Non-admin DELETE.ACCOUNT is blocked                      | Success | as expected                                            |
| Security: Non-admin cannot reach an account outside its allow list | Success | Access denied for account NEW_ACC: Not in allowed list |
| Security: Non-admin may reach its own account                      | Success | as expected                                            |
