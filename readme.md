# Aleo Mining Pool Server

## Introduction

A mining pool server for the Aleo network.

## Why a Standalone Server?

1. I want to separate the mining pool part from the network node as ledger syncing sometimes interferes with the mining pool operations.
2. I want to use a more efficient network protocol for pool - miner communication. 
3. Making too many changes to the snarkOS code could be a bad idea as I still need to sync with upstream code.
4. It's easier to test the mining pool with a standalone server.
5. It's also easier to add more features to a smaller codebase.

## Features

1. A stratum protocol. [Specs](stratum/spec.md).
2. A good enough automatic difficulty targeting system. (Needs more test under high load)
3. Stats for pool and provers.

## State

Undergoing a lot of rewrite:

- ~~Use RDBMS instead of RocksDB for most of the data storage~~
- ~~Use a real stratum protocol for pool - miner communication~~
- ~~Rework the difficulty targeting system~~
- Decide if more API endpoints are needed -- many of the work should be offloaded to frontends
- ~~Payout system step 1 - allocate rewards to provers after confirmation~~
- Payout system step 2 - send rewards to provers (indefinitely delayed until the network is ready)

### Things to test

* ~~If new blocks can be properly generated~~
* If the payout system works
* If the difficulty retargeting system works under high load - Works under light load
* If there is no deadlock under high load - Works under light load

## Usage

Don't use unless you know what you're doing for now.

### System requirements

- Rust 1.59+ (Not sure what's strictly required)

Optional:

- PostgreSQL 11+ (Still not sure what's strictly required)
- PL/Python 3.6+

## License

AGPL-3.0-or-later
